// Interactive TUI: diff rendering, patch prompts, editor integration.
//
// Derived from fastmod (Copyright Meta Platforms, Inc. and affiliates),
// used under the Apache License, Version 2.0. See LICENSE and NOTICE
// at the repo root for details.

use std::cmp::max;
use std::cmp::min;
use std::env;

use std::fs::read_to_string;
use std::io::Write as _;
use std::iter;
use std::path::Path;
use std::process::Command;
use std::process::Stdio;

use anyhow::Context as _;
use anyhow::Error;
use anyhow::bail;
use anyhow::ensure;
use regex::Regex;
use tempfile::NamedTempFile;

mod terminal;

use crate::diff::DiffResult;
use crate::ui::Color;
use crate::ui::Styles;

type Result<T> = ::std::result::Result<T, Error>;

#[derive(Copy, Clone)]
pub(crate) struct PreviewExpr<'a> {
    pub(crate) regex: &'a Regex,
    pub(crate) bytes_regex: &'a regex::bytes::Regex,
    pub(crate) replacer: &'a dyn Fn(&regex::Captures) -> String,
}

/// Convert a 0-based character offset to 0-based line number and column.
fn index_to_row_col(s: &str, index: usize) -> (usize, usize) {
    let chunk = &s.as_bytes()[..index];
    let line_num = memchr::memchr_iter(b'\n', chunk).count();
    let col = memchr::memrchr(b'\n', chunk).map_or(index, |newline| index - newline - 1);
    (line_num, col)
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
const INTERRUPTED: char = '\x03';

fn prompt(
    prompt_text: &str,
    letters: &str,
    default: Option<char>,
    is_first: bool,
    is_last: bool,
    styles: Styles<'_>,
) -> Result<char> {
    use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyModifiers};
    use crossterm::terminal::{disable_raw_mode, enable_raw_mode};

    let mut out = std::io::stdout().lock();
    write!(out, "{prompt_text}")?;
    out.flush()?;

    enable_raw_mode().context("Unable to enable raw mode")?;
    // Every exit from this loop, including a read failure, must pass the
    // `disable_raw_mode` below or the terminal is left without echo.
    let result = loop {
        let event = match event::read() {
            Ok(event) => event,
            Err(err) => break Err(anyhow::Error::new(err).context("Unable to read key event")),
        };
        if let Event::Key(KeyEvent {
            code, modifiers, ..
        }) = event
        {
            match code {
                KeyCode::Char('c') if modifiers.contains(KeyModifiers::CONTROL) => {
                    break Ok(INTERRUPTED);
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
        match c {
            NAV_BACK => write!(out, "{}\r", styles.paint(Color::Yellow, "←"))?,
            NAV_FORWARD => write!(out, "{}\r", styles.paint(Color::Yellow, "->"))?,
            'y' | 'A' => writeln!(out, "{}", styles.paint(Color::Green, c.to_string()))?,
            'n' | 'q' => writeln!(out, "{}", styles.paint(Color::Red, c.to_string()))?,
            'e' => writeln!(out, "{}", styles.paint(Color::Blue, c.to_string()))?,
            INTERRUPTED => {}
            _ => writeln!(out, "{c}")?,
        }
        out.flush()?;
    }
    result
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

const fn backward_to_char_boundary(s: &str, mut index: usize) -> usize {
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
    Quit,
    Interrupted,
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

#[derive(Clone)]
struct PreviewState {
    contents: String,
    windows: Vec<(usize, usize)>,
    expression_index: usize,
    edits: std::sync::Arc<[PlannedEdit]>,
    cursor: usize,
    removed: usize,
    inserted: usize,
}

#[derive(Clone)]
struct PlannedEdit {
    start: usize,
    end: usize,
    replacement: String,
    window_index: usize,
}

/// Match positions are planned once per expression against its input. Applying
/// an edit shifts later positions but never searches newly generated text.
struct PreviewSession<'a> {
    expressions: &'a [PreviewExpr<'a>],
    state: PreviewState,
    history: Vec<PreviewState>,
    record_history: bool,
}

impl<'a> PreviewSession<'a> {
    fn new(
        expressions: &'a [PreviewExpr<'a>],
        contents: String,
        windows: Vec<(usize, usize)>,
    ) -> Result<Self> {
        let mut session = Self {
            expressions,
            state: PreviewState {
                contents,
                windows,
                expression_index: 0,
                edits: Vec::new().into(),
                cursor: 0,
                removed: 0,
                inserted: 0,
            },
            history: Vec::new(),
            record_history: true,
        };
        session.plan(0)?;
        Ok(session)
    }

    fn plan(&mut self, resume: usize) -> Result<()> {
        let mut edits = Vec::new();
        self.state.edits = Vec::new().into();
        self.state.cursor = 0;
        self.state.removed = 0;
        self.state.inserted = 0;
        let Some(expr) = self.expressions.get(self.state.expression_index) else {
            return Ok(());
        };
        for (window_index, &(start, end)) in self.state.windows.iter().enumerate() {
            let window = &self.state.contents[start..end];
            for mat in expr.bytes_regex.find_iter(window.as_bytes()) {
                ensure!(
                    window.is_char_boundary(mat.start()) && window.is_char_boundary(mat.end()),
                    "preview cannot display a match splitting a UTF-8 character - use non-preview mode"
                );
            }
            for caps in expr.regex.captures_iter(&self.state.contents[start..end]) {
                let mat = caps.get(0).expect("full regex match is always present");
                if start + mat.start() < resume {
                    continue;
                }
                let replacement = (expr.replacer)(&caps);
                if replacement == mat.as_str() {
                    continue;
                }
                edits.push(PlannedEdit {
                    start: start + mat.start(),
                    end: start + mat.end(),
                    replacement,
                    window_index,
                });
            }
        }
        self.state.edits = edits.into();
        Ok(())
    }

    fn next(&mut self) -> Result<Option<PlannedEdit>> {
        loop {
            if let Some(edit) = self.state.edits.get(self.state.cursor) {
                let mut edit = edit.clone();
                edit.start = edit.start - self.state.removed + self.state.inserted;
                edit.end = edit.end - self.state.removed + self.state.inserted;
                return Ok(Some(edit));
            }
            self.state.expression_index += 1;
            if self.state.expression_index >= self.expressions.len() {
                return Ok(None);
            }
            self.plan(0)?;
        }
    }

    fn proposed(&self, edit: &PlannedEdit) -> String {
        let mut contents = self.state.contents.clone();
        contents.replace_range(edit.start..edit.end, &edit.replacement);
        contents
    }

    fn accept(&mut self, edit: &PlannedEdit, contents: String) {
        if self.record_history {
            self.history.push(self.state.clone());
        } else {
            self.history.clear();
        }
        let old_len = edit.end - edit.start;
        let new_len = edit.replacement.len();
        resize_window(&mut self.state.windows, edit.window_index, old_len, new_len);
        self.state.cursor += 1;
        self.state.removed += old_len;
        self.state.inserted += new_len;
        self.state.contents = contents;
    }

    fn reject(&mut self) {
        self.history.push(self.state.clone());
        self.state.cursor += 1;
    }

    fn undo(&mut self) -> Option<String> {
        let previous = self.history.pop()?;
        self.state = previous;
        Some(self.state.contents.clone())
    }

    fn edited(&mut self, contents: String, window_index: usize, resume: usize) -> Result<()> {
        resize_window(
            &mut self.state.windows,
            window_index,
            self.state.contents.len(),
            contents.len(),
        );
        for window in &mut self.state.windows {
            window.0 = to_char_boundary(&contents, window.0.min(contents.len()));
            window.1 = to_char_boundary(&contents, window.1.clamp(window.0, contents.len()));
        }
        let resume = to_char_boundary(&contents, resume.min(contents.len()));
        self.state.contents = contents;
        self.plan(resume)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PreviewOutcome {
    Continue,
    Quit,
    Interrupted,
}

fn resize_window(windows: &mut [(usize, usize)], index: usize, old_len: usize, new_len: usize) {
    if new_len >= old_len {
        let delta = new_len - old_len;
        windows[index].1 += delta;
        for window in &mut windows[index + 1..] {
            window.0 += delta;
            window.1 += delta;
        }
    } else {
        let delta = old_len - new_len;
        windows[index].1 = windows[index].1.saturating_sub(delta);
        for window in &mut windows[index + 1..] {
            window.0 = window.0.saturating_sub(delta);
            window.1 = window.1.saturating_sub(delta);
        }
    }
}

pub(crate) struct InteractivePatcher<'a> {
    styles: Styles<'a>,
    yes_to_all: bool,
    preview_tool: Option<String>,
    context_lines: usize,
}

impl<'theme> InteractivePatcher<'theme> {
    pub(crate) const fn new(
        accept_all: bool,
        preview_tool: Option<String>,
        context_lines: usize,
        styles: Styles<'theme>,
    ) -> Self {
        Self {
            styles,
            yes_to_all: accept_all,
            preview_tool,
            context_lines,
        }
    }

    /// Present expression matches in their original coordinate space. Each
    /// accepted decision is persisted before the next prompt.
    pub(crate) fn present_and_apply_patches_multi(
        &mut self,
        expressions: &[PreviewExpr<'_>],
        path: &Path,
        contents: String,
        windows: Vec<(usize, usize)>,
    ) -> Result<PreviewOutcome> {
        let mut session = PreviewSession::new(expressions, contents, windows)?;
        while let Some(edit) = session.next()? {
            let new_contents = session.proposed(&edit);
            let start_line = index_to_row_col(&session.state.contents, edit.start).0;
            let end_line = index_to_row_col(
                &session.state.contents,
                backward_to_char_boundary(
                    &session.state.contents,
                    edit.end.saturating_sub(usize::from(edit.end > edit.start)),
                ),
            )
            .0;
            let action = self.ask_about_patch(&PatchPrompt {
                path,
                old: &session.state.contents,
                new: &new_contents,
                start_line: start_line + 1,
                end_line: end_line + 1,
                match_index: session.state.cursor + 1,
                match_total: session.state.edits.len(),
                has_history: !session.history.is_empty(),
            })?;
            match action {
                PatchAction::Accept | PatchAction::AcceptAll | PatchAction::Edit => {
                    crate::write::commit(
                        path,
                        session.state.contents.as_bytes(),
                        new_contents.as_bytes(),
                    )?;
                    if matches!(action, PatchAction::AcceptAll) {
                        self.yes_to_all = true;
                    }
                    session.record_history = !self.yes_to_all;
                    session.accept(&edit, new_contents);
                    if matches!(action, PatchAction::Edit) {
                        run_editor(path, start_line + 1)?;
                        let contents = read_to_string(path)?;
                        session.edited(
                            contents,
                            edit.window_index,
                            edit.start + edit.replacement.len(),
                        )?;
                    }
                }
                PatchAction::Skip | PatchAction::Reject => session.reject(),
                PatchAction::Back => {
                    let expected = session.state.contents.clone();
                    if let Some(previous) = session.undo() {
                        crate::write::commit(path, expected.as_bytes(), previous.as_bytes())?;
                    }
                }
                PatchAction::Quit => return Ok(PreviewOutcome::Quit),
                PatchAction::Interrupted => return Ok(PreviewOutcome::Interrupted),
            }
        }
        Ok(PreviewOutcome::Continue)
    }

    fn ask_about_patch(&self, patch: &PatchPrompt<'_>) -> Result<PatchAction> {
        if self.yes_to_all {
            return Ok(PatchAction::Accept);
        }
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
        let diffs = Self::diffs_to_print(old, new, self.context_lines);
        if diffs.is_empty() {
            return Ok(PatchAction::Skip);
        }
        // Hide cursor during rendering to prevent it flashing at
        // intermediate positions while the diff is being painted.
        let cursor = terminal::HiddenCursor::new();

        // Clear after computing the diff so there's no visible gap
        terminal::clear();

        let display_path = path.to_string_lossy();
        let display_path = display_path.strip_prefix("./").unwrap_or(&display_path);
        let unbold = if self.styles.is_plain() {
            ""
        } else {
            "\x1b[22m"
        };
        if start_line == end_line {
            writeln!(
                std::io::stdout(),
                "{}{}{display_path}{}:{start_line}{}",
                self.styles.bold(),
                self.styles.fg(Color::Blue),
                unbold,
                self.styles.reset()
            )?;
        } else {
            writeln!(
                std::io::stdout(),
                "{}{}{display_path}{}:{start_line}-{end_line}{}",
                self.styles.bold(),
                self.styles.fg(Color::Blue),
                unbold,
                self.styles.reset()
            )?;
        }

        let diff_result = if let Some(ref preview_tool) = self.preview_tool {
            Self::run_external_diff(old, new, preview_tool)
        } else {
            crate::diff::print_diff(&diffs, self.styles).map_err(Into::into)
        };

        drop(cursor);
        diff_result?;

        let is_last = match_index >= match_total;
        let user_input = prompt(
            &format!(
                "\n{} {}{}{}{}{}{}{}{}{}{}{}{}{}{}{}",
                self.styles.bold_paint(
                    Color::Yellow,
                    format!("Accept [{match_index}/{match_total}]?")
                ),
                self.styles.bold_paint(Color::Green, "y"),
                self.styles.paint(Color::White, "es "),
                self.styles.paint(Color::Dim, "· "),
                self.styles.paint(Color::Red, "n"),
                self.styles.paint(Color::White, "o "),
                self.styles.paint(Color::Dim, "· "),
                self.styles.paint(Color::Blue, "e"),
                self.styles.paint(Color::White, "dit "),
                self.styles.paint(Color::Dim, "· "),
                self.styles.paint(Color::Green, "A"),
                self.styles.paint(Color::White, "ll "),
                self.styles.paint(Color::Dim, "· "),
                self.styles.paint(Color::Red, "q"),
                self.styles.paint(Color::White, "uit\n"),
                self.styles.paint(Color::Yellow, "❯ "),
            ),
            "yneAq",
            Some('y'),
            !has_history,
            is_last,
            self.styles,
        )?;
        match user_input {
            'y' => Ok(PatchAction::Accept),
            'n' => Ok(PatchAction::Reject),
            'e' => Ok(PatchAction::Edit),
            'A' => Ok(PatchAction::AcceptAll),
            'q' => Ok(PatchAction::Quit),
            INTERRUPTED => Ok(PatchAction::Interrupted),
            NAV_BACK => Ok(PatchAction::Back),
            NAV_FORWARD => Ok(PatchAction::Reject),
            _ => unreachable!(),
        }
    }

    fn diffs_to_print<'a>(
        orig: &'a str,
        edit: &'a str,
        context_lines: usize,
    ) -> Vec<DiffResult<&'a str>> {
        const fn is_same(x: &DiffResult<&str>) -> bool {
            matches!(x, DiffResult::Both(..))
        }

        let mut diffs = crate::diff::line_diffs(orig, edit);
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
        let size_of_up_context = min(size_of_context / 2, context_lines);
        let size_of_down_context = min(size_of_context / 2 + size_of_context % 2, context_lines);

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

    fn run_external_diff(old: &str, new: &str, preview_tool: &str) -> Result<()> {
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
    use super::{PreviewSession, backward_to_char_boundary, resize_window, to_char_boundary};
    use crate::expressions::{
        CompiledExpression, Expression, apply_compiled_expressions, compile_expressions,
    };
    use crate::options::MatchOptions;

    fn compiled(find: &str, replace: &str) -> Vec<CompiledExpression> {
        let options = MatchOptions {
            preview: true,
            regexp: true,
            greedy: true,
            expressions: vec![Expression {
                find: find.to_string(),
                replace: replace.to_string(),
            }],
            ..MatchOptions::default()
        };
        compile_expressions(&options).unwrap()
    }

    fn accepted_output(
        input: &str,
        expressions: &[CompiledExpression],
        windows: Vec<(usize, usize)>,
    ) -> String {
        let refs: Vec<_> = expressions
            .iter()
            .map(CompiledExpression::preview_expr)
            .collect();
        let mut session = PreviewSession::new(&refs, input.to_string(), windows).unwrap();
        while let Some(edit) = session.next().unwrap() {
            let proposal = session.proposed(&edit);
            session.accept(&edit, proposal);
        }
        session.state.contents
    }

    #[test]
    fn preview_accept_all_matches_bulk_boundaries_and_zero_width() {
        for (input, find, replace) in [
            ("", r"\z", "tail"),
            (
                "aa
", "^a", "b",
            ),
            ("aa", r"\ba", " "),
            ("ab", r"\z", "tail"),
            ("ab", r"(?:)", "_"),
            ("aba", "a*", "x"),
            ("foo.foo", "(foo)", "[$1]"),
            ("α α", "α", "αλφα"),
            ("aa", "a", "aa"),
        ] {
            let expressions = compiled(find, replace);
            let bulk = apply_compiled_expressions(input.as_bytes(), &expressions, false, &[]);
            assert_eq!(
                accepted_output(input, &expressions, vec![(0, input.len())]).as_bytes(),
                bulk.output.as_ref(),
                "pattern {find:?}"
            );
        }
    }

    #[test]
    fn preview_rejects_matches_splitting_utf8_without_changing_bulk_semantics() {
        let expressions = compiled("(?:)", "_");
        let refs: Vec<_> = expressions
            .iter()
            .map(CompiledExpression::preview_expr)
            .collect();
        let Err(error) = PreviewSession::new(&refs, "é".to_string(), vec![(0, 2)]) else {
            panic!("preview must reject byte boundaries inside a character");
        };
        assert_eq!(
            error.to_string(),
            "preview cannot display a match splitting a UTF-8 character - use non-preview mode"
        );
        let bulk = apply_compiled_expressions("é".as_bytes(), &expressions, false, &[]);
        assert_eq!(bulk.output.as_ref(), b"_\xc3_\xa9_");
    }

    #[test]
    fn preview_chains_expressions_and_respects_windows() {
        let mut expressions = compiled("a", "bb");
        expressions.extend(compiled("b", "c"));
        assert_eq!(
            accepted_output("a\na\na\n", &expressions, vec![(2, 4)]),
            "a\ncc\na\n"
        );
    }

    #[test]
    fn preview_reject_undo_and_editor_replan_restore_matching_state() {
        let expressions = compiled("a", "bb");
        let refs: Vec<_> = expressions
            .iter()
            .map(CompiledExpression::preview_expr)
            .collect();
        let mut session = PreviewSession::new(&refs, "a a".to_string(), vec![(0, 3)]).unwrap();
        let first = session.next().unwrap().unwrap();
        session.reject();
        assert_eq!(session.next().unwrap().unwrap().start, 2);
        assert_eq!(session.undo().unwrap(), "a a");
        assert_eq!(session.next().unwrap().unwrap().start, 0);
        let proposal = session.proposed(&first);
        session.accept(&first, proposal);
        assert_eq!(session.next().unwrap().unwrap().start, 3);
        session.edited("bb a a".to_string(), 0, 2).unwrap();
        assert_eq!(session.next().unwrap().unwrap().start, 3);
        while let Some(edit) = session.next().unwrap() {
            let proposal = session.proposed(&edit);
            session.accept(&edit, proposal);
        }
        assert_eq!(session.state.contents, "bb bb bb");
    }

    #[test]
    fn test_resize_window_shifts_following_windows() {
        let mut windows = vec![(3, 9), (12, 18)];
        resize_window(&mut windows, 0, 2, 5);
        assert_eq!(windows, vec![(3, 12), (15, 21)]);
        resize_window(&mut windows, 1, 4, 1);
        assert_eq!(windows, vec![(3, 12), (15, 18)]);
    }

    /// `to_char_boundary` advances forward past any mid-character byte indices,
    /// which `present_and_apply_patches_multi` relies on when bumping the
    /// search offset past a match whose end lands inside a multi-byte rune.
    #[test]
    fn test_to_char_boundary_advances_past_multibyte() {
        // "café" - 'é' is 2 bytes (0xc3 0xa9) at indices 3-4.
        let s = "café";
        assert_eq!(to_char_boundary(s, 0), 0);
        assert_eq!(to_char_boundary(s, 3), 3);
        assert_eq!(to_char_boundary(s, 4), 5); // mid-rune -> advances to end
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
        assert_eq!(backward_to_char_boundary(s, 4), 3); // mid-rune -> retreats
        assert_eq!(backward_to_char_boundary(s, 5), 5);
    }
}

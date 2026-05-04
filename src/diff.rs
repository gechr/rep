// Diff rendering: numbered file-level diff for the dry-run / apply path,
// plus a hunk-level diff for the interactive preview.
//
// Inline highlighting in the numbered diff is *span-driven*: the input and
// output byte spans of every replacement are recorded by the replace step
// (`apply_compiled_expressions`) and threaded through here verbatim. There is
// no token-level or character-level guessing about what changed - the
// replacer is the source of truth for that.

use std::fmt::Write as _;
use std::io::Write as _;

use diff::Result as DiffResult;

use crate::expressions::Replacement;
use crate::ui::Color;
use crate::ui::Styles;

#[derive(Clone, Copy)]
pub(crate) struct DiffHints<'a> {
    pub(crate) spans: &'a [Replacement],
    pub(crate) linewise: bool,
    pub(crate) multiline_spans: bool,
}

#[derive(Clone, Copy)]
struct Hyperlinks<'a> {
    format: Option<&'a str>,
    path: &'a str,
    /// 1-indexed line -> first-match column. `None` when not tracked; lookups
    /// for absent lines fall back to column 1 inside `hyperlink_url`.
    columns: Option<&'a std::collections::HashMap<usize, usize>>,
}

impl<'a> Hyperlinks<'a> {
    const fn new(
        format: Option<&'a str>,
        path: &'a str,
        columns: Option<&'a std::collections::HashMap<usize, usize>>,
    ) -> Self {
        Self {
            format,
            path,
            columns,
        }
    }

    fn write(self, out: &mut String, line: usize, text: &str) {
        let Some(format) = self.format else {
            out.push_str(text);
            return;
        };
        let column = self
            .columns
            .and_then(|m| m.get(&line).copied())
            .unwrap_or(0);
        out.push_str(&crate::osc8(
            &crate::hyperlink_url(format, self.path, line, column),
            text,
        ));
    }
}

pub(crate) fn print_file_line_diff(
    old: &str,
    new: &str,
    hints: DiffHints<'_>,
    styles: Styles,
    hyperlink_format: Option<&str>,
    hyperlink_path: &str,
    columns: &std::collections::HashMap<usize, usize>,
) {
    let columns = (!columns.is_empty()).then_some(columns);
    let hyperlinks = Hyperlinks::new(hyperlink_format, hyperlink_path, columns);
    let spans = hints.spans;
    let old_line_spans = group_spans_by_line(old, spans, SpanSide::Input);
    let new_line_spans = group_spans_by_line(new, spans, SpanSide::Output);
    let span_highlighting = !spans.is_empty();

    if span_highlighting
        && replacements_preserve_line_boundaries(old, new, spans)
        && print_same_line_span_diff(
            old,
            new,
            styles,
            hyperlinks,
            &old_line_spans,
            &new_line_spans,
        )
    {
        return;
    }
    if hints.linewise
        && print_linewise_diff(
            old,
            new,
            styles,
            hyperlinks,
            span_highlighting,
            &old_line_spans,
            &new_line_spans,
        )
    {
        return;
    }
    if hints.multiline_spans
        && print_multiline_span_diff(
            old,
            new,
            spans,
            styles,
            hyperlinks,
            &old_line_spans,
            &new_line_spans,
        )
    {
        return;
    }

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

    let mut out = String::new();
    let mut writer = NumberedDiffWriter {
        out: &mut out,
        width,
        styles,
        hyperlinks,
        span_highlighting,
        old_line_spans: &old_line_spans,
        new_line_spans: &new_line_spans,
    };
    for (old_lines, new_lines) in blocks {
        writer.write_block(&old_lines, &new_lines);
    }
    write_stdout(&out);
}

fn replacements_preserve_line_boundaries(old: &str, new: &str, spans: &[Replacement]) -> bool {
    !spans.is_empty()
        && spans.iter().all(|span| {
            span.input_len > 0
                && span.output_len > 0
                && !old.as_bytes()[span.input_start..span.input_end()].contains(&b'\n')
                && !new.as_bytes()[span.output_start..span.output_end()].contains(&b'\n')
        })
}

fn print_same_line_span_diff(
    old: &str,
    new: &str,
    styles: Styles,
    hyperlinks: Hyperlinks<'_>,
    old_line_spans: &std::collections::HashMap<usize, Vec<LocalSpan>>,
    new_line_spans: &std::collections::HashMap<usize, Vec<LocalSpan>>,
) -> bool {
    let mut changed_lines: Vec<usize> = old_line_spans
        .keys()
        .chain(new_line_spans.keys())
        .copied()
        .collect();
    changed_lines.sort_unstable();
    changed_lines.dedup();
    if changed_lines.is_empty() {
        return false;
    }

    let Some(old_lines) = lines_for_numbers(old, &changed_lines) else {
        return false;
    };
    let Some(new_lines) = lines_for_numbers(new, &changed_lines) else {
        return false;
    };

    let width = changed_lines
        .iter()
        .map(|line_no| line_no.to_string().len())
        .max()
        .unwrap_or(1);
    let mut out = String::new();
    let mut writer = NumberedDiffWriter {
        out: &mut out,
        width,
        styles,
        hyperlinks,
        span_highlighting: true,
        old_line_spans,
        new_line_spans,
    };

    let mut block_start = 0;
    for idx in 1..=changed_lines.len() {
        if idx == changed_lines.len() || changed_lines[idx] != changed_lines[idx - 1] + 1 {
            writer.write_block(&old_lines[block_start..idx], &new_lines[block_start..idx]);
            block_start = idx;
        }
    }
    write_stdout(&out);
    true
}

fn print_linewise_diff(
    old: &str,
    new: &str,
    styles: Styles,
    hyperlinks: Hyperlinks<'_>,
    span_highlighting: bool,
    old_line_spans: &std::collections::HashMap<usize, Vec<LocalSpan>>,
    new_line_spans: &std::collections::HashMap<usize, Vec<LocalSpan>>,
) -> bool {
    let old_lines: Vec<&str> = old.lines().collect();
    let new_lines: Vec<&str> = new.lines().collect();
    if old_lines.len() != new_lines.len() {
        return false;
    }

    let mut changed_lines = Vec::new();
    for (idx, (old_line, new_line)) in old_lines.iter().zip(&new_lines).enumerate() {
        if old_line != new_line {
            changed_lines.push(idx + 1);
        }
    }
    if changed_lines.is_empty() {
        return false;
    }

    let width = changed_lines
        .iter()
        .map(|line_no| line_no.to_string().len())
        .max()
        .unwrap_or(1);
    let mut out = String::new();
    let mut writer = NumberedDiffWriter {
        out: &mut out,
        width,
        styles,
        hyperlinks,
        span_highlighting,
        old_line_spans,
        new_line_spans,
    };

    let mut block_start = 0;
    for idx in 1..=changed_lines.len() {
        if idx == changed_lines.len() || changed_lines[idx] != changed_lines[idx - 1] + 1 {
            let old_block: Vec<_> = changed_lines[block_start..idx]
                .iter()
                .map(|line_no| (*line_no, old_lines[*line_no - 1]))
                .collect();
            let new_block: Vec<_> = changed_lines[block_start..idx]
                .iter()
                .map(|line_no| (*line_no, new_lines[*line_no - 1]))
                .collect();
            writer.write_block(&old_block, &new_block);
            block_start = idx;
        }
    }
    write_stdout(&out);
    true
}

fn print_multiline_span_diff(
    old: &str,
    new: &str,
    spans: &[Replacement],
    styles: Styles,
    hyperlinks: Hyperlinks<'_>,
    old_line_spans: &std::collections::HashMap<usize, Vec<LocalSpan>>,
    new_line_spans: &std::collections::HashMap<usize, Vec<LocalSpan>>,
) -> bool {
    let Some(mut hunks) = multiline_span_hunks(old, new, spans) else {
        return false;
    };
    if hunks.is_empty() {
        return false;
    }

    let width = hunks
        .iter()
        .flat_map(|hunk| {
            hunk.old_lines
                .iter()
                .chain(&hunk.new_lines)
                .map(|(line_no, _)| line_no.to_string().len())
        })
        .max()
        .unwrap_or(1);
    let mut out = String::new();
    let mut writer = NumberedDiffWriter {
        out: &mut out,
        width,
        styles,
        hyperlinks,
        span_highlighting: true,
        old_line_spans,
        new_line_spans,
    };
    for hunk in &mut hunks {
        writer.write_block(&hunk.old_lines, &hunk.new_lines);
    }
    write_stdout(&out);
    true
}

struct SpanHunk<'a> {
    old_lines: Vec<(usize, &'a str)>,
    new_lines: Vec<(usize, &'a str)>,
}

fn multiline_span_hunks<'a>(
    old: &'a str,
    new: &'a str,
    spans: &[Replacement],
) -> Option<Vec<SpanHunk<'a>>> {
    if spans.is_empty()
        || spans
            .iter()
            .any(|span| span.input_len == 0 || span.output_len == 0)
    {
        return None;
    }

    let old_index = LineIndex::new(old);
    let new_index = LineIndex::new(new);
    let mut hunks = Vec::new();
    let mut current: Option<HunkRange> = None;
    let mut sorted: Vec<_> = spans.iter().collect();
    sorted.sort_unstable_by_key(|span| span.input_start);

    for span in sorted {
        let old_start = old_index.line_start_for_byte(span.input_start)?;
        let old_end = old_index.line_end_for_byte(span.input_end())?;
        let new_start = span
            .output_start
            .checked_sub(span.input_start.checked_sub(old_start)?)?;
        let new_end = span.output_end().checked_add(old_end - span.input_end())?;
        if new_end > new.len() {
            return None;
        }

        match &mut current {
            Some(range) if old_start <= range.old_end.saturating_add(1) => {
                range.old_end = old_end;
                range.new_end = range.new_end.max(new_end);
            }
            Some(range) => {
                hunks.push(range.to_hunk(old, new, &old_index, &new_index)?);
                current = Some(HunkRange {
                    old_start,
                    old_end,
                    new_start,
                    new_end,
                });
            }
            None => {
                current = Some(HunkRange {
                    old_start,
                    old_end,
                    new_start,
                    new_end,
                });
            }
        }
    }
    if let Some(range) = current {
        hunks.push(range.to_hunk(old, new, &old_index, &new_index)?);
    }
    Some(hunks)
}

struct HunkRange {
    old_start: usize,
    old_end: usize,
    new_start: usize,
    new_end: usize,
}

impl HunkRange {
    fn to_hunk<'a>(
        &self,
        old: &'a str,
        new: &'a str,
        old_index: &LineIndex,
        new_index: &LineIndex,
    ) -> Option<SpanHunk<'a>> {
        Some(SpanHunk {
            old_lines: numbered_lines(
                &old[self.old_start..self.old_end],
                old_index.line_no_for_byte(self.old_start)?,
            ),
            new_lines: numbered_lines(
                &new[self.new_start..self.new_end],
                new_index.line_no_for_byte(self.new_start)?,
            ),
        })
    }
}

struct LineIndex {
    starts: Vec<usize>,
    len: usize,
}

impl LineIndex {
    fn new(text: &str) -> Self {
        let mut starts = vec![0];
        for (idx, byte) in text.as_bytes().iter().enumerate() {
            if *byte == b'\n' {
                starts.push(idx + 1);
            }
        }
        Self {
            starts,
            len: text.len(),
        }
    }

    fn line_no_for_byte(&self, byte: usize) -> Option<usize> {
        (byte <= self.len).then(|| self.starts.partition_point(|start| *start <= byte))
    }

    fn line_start_for_byte(&self, byte: usize) -> Option<usize> {
        let line_no = self.line_no_for_byte(byte)?;
        self.starts.get(line_no - 1).copied()
    }

    fn line_end_for_byte(&self, byte: usize) -> Option<usize> {
        let line_no = self.line_no_for_byte(byte)?;
        Some(
            self.starts
                .get(line_no)
                .map_or(self.len, |next_start| next_start - 1),
        )
    }
}

fn numbered_lines(text: &str, start_line_no: usize) -> Vec<(usize, &str)> {
    text.split('\n')
        .enumerate()
        .map(|(idx, line)| (start_line_no + idx, line))
        .collect()
}

fn lines_for_numbers<'a>(text: &'a str, line_numbers: &[usize]) -> Option<Vec<(usize, &'a str)>> {
    let mut out = Vec::with_capacity(line_numbers.len());
    let mut wanted = line_numbers.iter().copied().peekable();
    for (idx, line) in text.lines().enumerate() {
        let line_no = idx + 1;
        while wanted.next_if_eq(&line_no).is_some() {
            out.push((line_no, line));
        }
        if wanted.peek().is_none() {
            return Some(out);
        }
    }
    None
}

pub(crate) fn print_diff(diffs: &[DiffResult<&str>], styles: Styles) {
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

fn print_inline_diff(old_line: &str, new_line: &str, styles: Styles) {
    let mut out = String::new();
    let inline = inline_token_diff(old_line, new_line);
    let _ = write!(out, "{}- {}", styles.fg(Color::Red), styles.reset());
    write_inline_chars(&mut out, &inline, InlineSide::Old, styles);
    out.push('\n');

    let _ = write!(out, "{}+ {}", styles.fg(Color::Green), styles.reset());
    write_inline_chars(&mut out, &inline, InlineSide::New, styles);
    out.push('\n');
    write_stdout(&out);
}

fn write_stdout(out: &str) {
    let mut stdout = std::io::stdout().lock();
    if stdout.write_all(out.as_bytes()).is_err() {}
}

struct NumberedDiffWriter<'a, 'b> {
    out: &'a mut String,
    width: usize,
    styles: Styles,
    hyperlinks: Hyperlinks<'b>,
    /// True when replacements were tracked for this file. Lines without a
    /// visible span on this side are rendered plainly instead of whole-line
    /// colored; that represents insertions on the output side and deletions on
    /// the input side without emitting invisible zero-width highlights.
    span_highlighting: bool,
    /// 1-indexed line -> sorted local-byte spans for input (old) lines.
    old_line_spans: &'a std::collections::HashMap<usize, Vec<LocalSpan>>,
    /// 1-indexed line -> sorted local-byte spans for output (new) lines.
    new_line_spans: &'a std::collections::HashMap<usize, Vec<LocalSpan>>,
}

impl NumberedDiffWriter<'_, '_> {
    fn write_block(&mut self, old_lines: &[(usize, &str)], new_lines: &[(usize, &str)]) {
        let paired = old_lines.len().min(new_lines.len());
        for idx in 0..paired {
            let (old_line_no, old_line) = old_lines[idx];
            let (new_line_no, new_line) = new_lines[idx];
            self.write_line(old_line_no, '-', old_line, Color::Red, SpanSide::Input);
            self.write_line(new_line_no, '+', new_line, Color::Green, SpanSide::Output);
        }
        for (line_no, line) in &old_lines[paired..] {
            self.write_line(*line_no, '-', line, Color::Red, SpanSide::Input);
        }
        for (line_no, line) in &new_lines[paired..] {
            self.write_line(*line_no, '+', line, Color::Green, SpanSide::Output);
        }
    }

    fn write_line(&mut self, line_no: usize, sign: char, line: &str, color: Color, side: SpanSide) {
        self.write_prefix(line_no, sign, color);
        let spans = match side {
            SpanSide::Input => self.old_line_spans.get(&line_no),
            SpanSide::Output => self.new_line_spans.get(&line_no),
        };
        match spans {
            Some(spans) if !spans.is_empty() => {
                render_line_with_spans(self.out, line, spans, color, self.styles);
            }
            _ if self.span_highlighting => {
                self.out.push_str(line);
            }
            _ => {
                let _ = write!(
                    self.out,
                    "{}{line}{}",
                    self.styles.fg(color),
                    self.styles.reset(),
                );
            }
        }
        self.out.push('\n');
    }

    fn write_prefix(&mut self, line_no: usize, sign: char, line_color: Color) {
        let line_no_text = line_no.to_string();
        let padding = " ".repeat(self.width.saturating_sub(line_no_text.len()));
        let _ = write!(
            self.out,
            "{}{}{}",
            self.styles.dim(),
            self.styles.fg(line_color),
            padding,
        );
        self.hyperlinks.write(self.out, line_no, &line_no_text);
        self.out.push_str(self.styles.reset());
        if self.styles.is_plain() {
            let _ = write!(self.out, "{sign} ");
        } else {
            self.out.push(' ');
        }
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

#[derive(Clone, Copy)]
enum InlineSide {
    Old,
    New,
}

#[derive(Clone, Copy)]
enum TokenDiff<'a> {
    Both(&'a str, &'a str),
    Left(&'a str),
    Right(&'a str),
}

fn inline_token_diff<'a>(old_line: &'a str, new_line: &'a str) -> Vec<TokenDiff<'a>> {
    let old_tokens = tokenize(old_line);
    let new_tokens = tokenize(new_line);
    diff::slice(&old_tokens, &new_tokens)
        .into_iter()
        .map(|item| match item {
            DiffResult::Both(old, new) => TokenDiff::Both(old, new),
            DiffResult::Left(old) => TokenDiff::Left(old),
            DiffResult::Right(new) => TokenDiff::Right(new),
        })
        .collect()
}

fn write_inline_chars(out: &mut String, diffs: &[TokenDiff<'_>], side: InlineSide, styles: Styles) {
    let mut i = 0;
    while i < diffs.len() {
        match diffs[i] {
            TokenDiff::Both(old, new) => {
                let tok = match side {
                    InlineSide::Old => old,
                    InlineSide::New => new,
                };
                out.push_str(tok);
                i += 1;
            }
            TokenDiff::Left(_) | TokenDiff::Right(_) => {
                let mut lefts: Vec<&str> = Vec::new();
                let mut rights: Vec<&str> = Vec::new();
                while i < diffs.len() {
                    match diffs[i] {
                        TokenDiff::Left(t) => {
                            lefts.push(t);
                            i += 1;
                        }
                        TokenDiff::Right(t) => {
                            rights.push(t);
                            i += 1;
                        }
                        TokenDiff::Both(..) => break,
                    }
                }
                write_change_block(out, &lefts, &rights, side, styles);
            }
        }
    }
}

fn write_change_block(
    out: &mut String,
    lefts: &[&str],
    rights: &[&str],
    side: InlineSide,
    styles: Styles,
) {
    let balanced = lefts.len() == rights.len();
    if balanced {
        write_balanced_change_block(out, lefts, rights, side, styles);
        return;
    }

    let old_text = lefts.concat();
    let new_text = rights.concat();
    if should_block_char_diff(&old_text, &new_text) {
        write_char_diff(out, &old_text, &new_text, side, styles);
        return;
    }

    let (own_tokens, own_color) = match side {
        InlineSide::Old => (lefts, Color::Red),
        InlineSide::New => (rights, Color::Green),
    };
    write_underlined_tokens(out, own_tokens, own_color, styles);
}

fn write_balanced_change_block(
    out: &mut String,
    lefts: &[&str],
    rights: &[&str],
    side: InlineSide,
    styles: Styles,
) {
    let (own_tokens, own_color) = match side {
        InlineSide::Old => (lefts, Color::Red),
        InlineSide::New => (rights, Color::Green),
    };
    for (k, own_tok) in own_tokens.iter().enumerate() {
        if should_intra_word_diff(lefts[k], rights[k]) {
            write_char_diff(out, lefts[k], rights[k], side, styles);
        } else {
            let _ = write!(
                out,
                "{}{}{own_tok}{}",
                styles.fg(own_color),
                styles.underline(),
                styles.reset(),
            );
        }
    }
}

fn write_underlined_tokens(out: &mut String, tokens: &[&str], color: Color, styles: Styles) {
    for token in tokens {
        let _ = write!(
            out,
            "{}{}{token}{}",
            styles.fg(color),
            styles.underline(),
            styles.reset(),
        );
    }
}

fn write_char_diff(out: &mut String, old: &str, new: &str, side: InlineSide, styles: Styles) {
    let color = match side {
        InlineSide::Old => Color::Red,
        InlineSide::New => Color::Green,
    };
    let mut highlighting = false;
    for item in diff::chars(old, new) {
        match (side, item) {
            (InlineSide::Old, DiffResult::Both(ch, _))
            | (InlineSide::New, DiffResult::Both(_, ch)) => {
                if highlighting {
                    out.push_str(styles.reset());
                    highlighting = false;
                }
                out.push(ch);
            }
            (InlineSide::Old, DiffResult::Left(ch)) | (InlineSide::New, DiffResult::Right(ch)) => {
                if !highlighting {
                    let _ = write!(out, "{}{}", styles.fg(color), styles.underline());
                    highlighting = true;
                }
                out.push(ch);
            }
            _ => {}
        }
    }
    if highlighting {
        out.push_str(styles.reset());
    }
}

fn should_block_char_diff(old: &str, new: &str) -> bool {
    const MAX_BLOCK_CHAR_DIFF_LEN: usize = 1024;
    if old.len() > MAX_BLOCK_CHAR_DIFF_LEN || new.len() > MAX_BLOCK_CHAR_DIFF_LEN {
        return false;
    }
    has_single_changed_run_per_side(old, new)
}

pub(crate) fn should_intra_word_diff(old_tok: &str, new_tok: &str) -> bool {
    // Cap diff work for pathological tokens (e.g. a multi-KB minified identifier).
    const MAX_INTRA_WORD_LEN: usize = 1024;
    if token_kind(old_tok) != TokenKind::Word || token_kind(new_tok) != TokenKind::Word {
        return false;
    }
    if old_tok.len() > MAX_INTRA_WORD_LEN || new_tok.len() > MAX_INTRA_WORD_LEN {
        return false;
    }
    // Use char-diff only when each side forms at most one contiguous changed
    // run, so colored characters are never interrupted by uncolored shared
    // chars. Two-or-more runs on either side would speckle the output.
    has_single_changed_run_per_side(old_tok, new_tok)
}

fn has_single_changed_run_per_side(old: &str, new: &str) -> bool {
    let mut left_runs = 0usize;
    let mut right_runs = 0usize;
    let mut in_left = false;
    let mut in_right = false;
    for item in diff::chars(old, new) {
        match item {
            DiffResult::Left(_) => {
                if !in_left {
                    left_runs += 1;
                    in_left = true;
                }
                in_right = false;
            }
            DiffResult::Right(_) => {
                if !in_right {
                    right_runs += 1;
                    in_right = true;
                }
                in_left = false;
            }
            DiffResult::Both(..) => {
                in_left = false;
                in_right = false;
            }
        }
        if left_runs > 1 || right_runs > 1 {
            return false;
        }
    }
    true
}

#[derive(Clone, Copy, PartialEq)]
enum TokenKind {
    Whitespace,
    Word,
    Symbol,
}

fn token_kind(tok: &str) -> TokenKind {
    let Some(c) = tok.chars().next() else {
        return TokenKind::Symbol;
    };
    classify(c)
}

fn classify(c: char) -> TokenKind {
    if c.is_whitespace() {
        TokenKind::Whitespace
    } else if c.is_alphanumeric() {
        TokenKind::Word
    } else {
        TokenKind::Symbol
    }
}

pub(crate) fn tokenize(line: &str) -> Vec<&str> {
    let chars: Vec<(usize, char)> = line.char_indices().collect();
    let n = chars.len();
    let byte_at = |k: usize| -> usize { if k < n { chars[k].0 } else { line.len() } };
    let mut tokens = Vec::new();
    let mut i = 0;
    while i < n {
        let c = chars[i].1;
        match classify(c) {
            TokenKind::Symbol => {
                tokens.push(&line[chars[i].0..byte_at(i + 1)]);
                i += 1;
            }
            TokenKind::Whitespace => {
                let mut j = i + 1;
                while j < n && classify(chars[j].1) == TokenKind::Whitespace {
                    j += 1;
                }
                tokens.push(&line[chars[i].0..byte_at(j)]);
                i = j;
            }
            TokenKind::Word => {
                let mut j = i + 1;
                while j < n && classify(chars[j].1) == TokenKind::Word {
                    j += 1;
                }
                let mut sub_start = i;
                let mut k = i + 1;
                while k < j {
                    let prev = chars[k - 1].1;
                    let cur = chars[k].1;
                    let next = chars.get(k + 1).map(|x| x.1);
                    if is_subword_boundary(prev, cur, next) {
                        tokens.push(&line[chars[sub_start].0..chars[k].0]);
                        sub_start = k;
                    }
                    k += 1;
                }
                tokens.push(&line[chars[sub_start].0..byte_at(j)]);
                i = j;
            }
        }
    }
    tokens
}

fn is_subword_boundary(prev: char, cur: char, next: Option<char>) -> bool {
    if prev.is_alphabetic() != cur.is_alphabetic() {
        return true;
    }
    if prev.is_lowercase() && cur.is_uppercase() {
        return true;
    }
    if prev.is_uppercase()
        && cur.is_uppercase()
        && let Some(n) = next
        && n.is_lowercase()
    {
        return true;
    }
    false
}

/// A span trimmed to a single line, expressed in line-local byte offsets
/// relative to the start of the line text (which excludes the trailing `\n`).
#[derive(Clone, Copy, Debug)]
struct LocalSpan {
    start: usize,
    end: usize,
}

#[derive(Clone, Copy)]
enum SpanSide {
    Input,
    Output,
}

/// Walk `text` once, splitting each replacement span at every `\n` it crosses
/// and bucketing per-line slices into a 1-indexed `line -> Vec<LocalSpan>`
/// map. Empty spans (insertions on input side, deletions on output side) are
/// dropped: an empty underline is invisible and would emit useless escapes.
fn group_spans_by_line(
    text: &str,
    spans: &[Replacement],
    side: SpanSide,
) -> std::collections::HashMap<usize, Vec<LocalSpan>> {
    let mut map: std::collections::HashMap<usize, Vec<LocalSpan>> =
        std::collections::HashMap::new();
    if spans.is_empty() {
        return map;
    }

    let bytes = text.as_bytes();
    // Compute the start byte of each line lazily as we iterate spans in
    // order. Because spans from `apply_compiled_expressions` are produced
    // left-to-right by the regex engine, they're already sorted by start
    // offset on both sides; we exploit that to walk the buffer once.
    let mut line_no: usize = 1;
    let mut line_start: usize = 0;

    let mut sorted: Vec<(usize, usize)> = spans
        .iter()
        .map(|s| match side {
            SpanSide::Input => (s.input_start, s.input_end()),
            SpanSide::Output => (s.output_start, s.output_end()),
        })
        .filter(|(start, end)| end > start)
        .collect();
    sorted.sort_unstable_by_key(|&(start, _)| start);

    for (mut start, end) in sorted {
        // Advance past lines that end before the span starts.
        while line_start < bytes.len() {
            let nl = memchr::memchr(b'\n', &bytes[line_start..]).map(|i| line_start + i);
            match nl {
                Some(nl_pos) if nl_pos < start => {
                    line_no += 1;
                    line_start = nl_pos + 1;
                }
                _ => break,
            }
        }
        // Now `line_start <= start` and either there is no further newline
        // before `start` or it sits past it. Slice the span line by line.
        while start < end {
            let nl = memchr::memchr(b'\n', &bytes[line_start..]).map(|i| line_start + i);
            let line_end_byte = nl.unwrap_or(bytes.len());
            let chunk_end = end.min(line_end_byte);
            if chunk_end > start {
                let local_start = start - line_start;
                let local_end = chunk_end - line_start;
                map.entry(line_no).or_default().push(LocalSpan {
                    start: local_start,
                    end: local_end,
                });
            }
            if chunk_end == end {
                break;
            }
            // Crossed a newline: advance to the next line and continue with
            // whatever's left of the span.
            debug_assert!(nl.is_some(), "chunk_end < end requires a newline ahead");
            line_no += 1;
            line_start = line_end_byte + 1;
            start = line_start;
        }
    }

    map
}

/// Render one line, underlining every span in `color`. Spans are assumed
/// sorted and non-overlapping. UTF-8 boundaries are validated before
/// slicing; if a span lands mid-codepoint (possible when `regex::bytes`
/// matches non-UTF-8 byte sequences in a file that's otherwise valid UTF-8),
/// we fall back to coloring the whole line so the user still sees that it
/// changed - at the cost of inline precision on that one line.
fn render_line_with_spans(
    out: &mut String,
    line: &str,
    spans: &[LocalSpan],
    color: Color,
    styles: Styles,
) {
    if !spans
        .iter()
        .all(|s| line.is_char_boundary(s.start) && line.is_char_boundary(s.end))
    {
        let _ = write!(out, "{}{line}{}", styles.fg(color), styles.reset());
        return;
    }

    let mut cursor = 0;
    for span in spans {
        if span.start < cursor {
            // Defensive: overlapping or out-of-order spans shouldn't happen
            // (the replace step produces left-to-right non-overlapping
            // matches), but skip rather than panic if they do.
            continue;
        }
        out.push_str(&line[cursor..span.start]);
        let _ = write!(
            out,
            "{}{}{}{}",
            styles.fg(color),
            styles.underline(),
            &line[span.start..span.end],
            styles.reset(),
        );
        cursor = span.end;
    }
    out.push_str(&line[cursor..]);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rep(
        input_start: usize,
        input_len: usize,
        output_start: usize,
        output_len: usize,
    ) -> Replacement {
        Replacement {
            input_start,
            input_len,
            output_start,
            output_len,
        }
    }

    #[test]
    fn group_spans_by_line_buckets_input_spans_per_line() {
        // "foo\nbar\nbaz\n" - replace "foo"@0 and "bar"@4.
        let text = "foo\nbar\nbaz\n";
        let spans = vec![rep(0, 3, 0, 3), rep(4, 3, 4, 3)];
        let map = group_spans_by_line(text, &spans, SpanSide::Input);
        assert_eq!(map.get(&1).unwrap().len(), 1);
        assert_eq!(map.get(&1).unwrap()[0].start, 0);
        assert_eq!(map.get(&1).unwrap()[0].end, 3);
        assert_eq!(map.get(&2).unwrap()[0].start, 0);
        assert_eq!(map.get(&2).unwrap()[0].end, 3);
        assert!(map.get(&3).is_none());
    }

    #[test]
    fn group_spans_by_line_handles_multi_line_match_by_splitting() {
        // Span covers "oo\nba" (bytes 1..6 of "foo\nbar\nbaz") - two lines.
        let text = "foo\nbar\nbaz";
        let spans = vec![rep(1, 5, 1, 5)];
        let map = group_spans_by_line(text, &spans, SpanSide::Input);
        // line 1: "foo" - span covers bytes 1..3 locally.
        let l1 = &map[&1];
        assert_eq!(l1.len(), 1);
        assert_eq!((l1[0].start, l1[0].end), (1, 3));
        // line 2: "bar" - span covers bytes 0..2 locally.
        let l2 = &map[&2];
        assert_eq!(l2.len(), 1);
        assert_eq!((l2[0].start, l2[0].end), (0, 2));
    }

    #[test]
    fn group_spans_by_line_drops_zero_length_spans() {
        let text = "foo\nbar";
        // Pure deletion on input (output_len=0 doesn't matter for input side
        // mapping); pure insertion on output side - both should drop because
        // the relevant side has zero length.
        let deletion = rep(0, 0, 0, 3); // input zero-length: skipped from input map
        let insertion = rep(0, 3, 0, 0); // output zero-length: skipped from output map
        let input_map = group_spans_by_line(text, &[deletion], SpanSide::Input);
        assert!(input_map.is_empty());
        let output_map = group_spans_by_line(text, &[insertion], SpanSide::Output);
        assert!(output_map.is_empty());
    }

    #[test]
    fn multiline_span_hunks_merges_adjacent_lines_from_unsorted_spans() {
        let old = "α static ω\nβ static δ\nkeep\n";
        let new = "α STATIC\n ω\nβ STATIC\n δ\nkeep\n";
        let spans = vec![rep(16, 6, 17, 7), rep(3, 6, 3, 7)];

        let hunks = multiline_span_hunks(old, new, &spans).unwrap();

        assert_eq!(hunks.len(), 1);
        assert_eq!(
            hunks[0].old_lines,
            vec![(1, "α static ω"), (2, "β static δ")]
        );
        assert_eq!(
            hunks[0].new_lines,
            vec![(1, "α STATIC"), (2, " ω"), (3, "β STATIC"), (4, " δ")]
        );
    }

    #[test]
    fn multiline_span_hunks_handles_file_edges_without_trailing_newline() {
        let old = "static α\nkeep\nω static";
        let new = "STATIC\n α\nkeep\nω STATIC\n";
        let spans = vec![rep(0, 6, 0, 7), rep(18, 6, 19, 7)];

        let hunks = multiline_span_hunks(old, new, &spans).unwrap();

        assert_eq!(hunks.len(), 2);
        assert_eq!(hunks[0].old_lines, vec![(1, "static α")]);
        assert_eq!(hunks[0].new_lines, vec![(1, "STATIC"), (2, " α")]);
        assert_eq!(hunks[1].old_lines, vec![(3, "ω static")]);
        assert_eq!(hunks[1].new_lines, vec![(4, "ω STATIC"), (5, "")]);
    }

    #[test]
    fn render_line_with_spans_underlines_each_span() {
        let line = "output.status.success";
        let spans = vec![
            LocalSpan { start: 6, end: 7 },
            LocalSpan { start: 13, end: 14 },
        ];
        let mut out = String::new();
        render_line_with_spans(&mut out, line, &spans, Color::Red, Styles::ansi());
        assert_eq!(
            out,
            "output\x1b[31m\x1b[4m.\x1b[mstatus\x1b[31m\x1b[4m.\x1b[msuccess",
        );
    }

    #[test]
    fn render_line_with_spans_falls_back_when_span_lands_mid_codepoint() {
        // "café" - 'é' is 2 bytes (0xC3 0xA9) at byte offset 3..5.
        // A span covering only byte 4 lands mid-codepoint and would panic
        // when slicing as `&str`; instead we color the whole line.
        let line = "café";
        let spans = vec![LocalSpan { start: 4, end: 5 }];
        let mut out = String::new();
        render_line_with_spans(&mut out, line, &spans, Color::Red, Styles::ansi());
        assert_eq!(out, "\x1b[31mcafé\x1b[m");
    }

    #[test]
    fn render_line_with_spans_with_no_spans_writes_nothing_extra() {
        let line = "unchanged";
        let mut out = String::new();
        render_line_with_spans(&mut out, line, &[], Color::Green, Styles::ansi());
        assert_eq!(out, "unchanged");
    }

    #[test]
    fn numbered_writer_leaves_empty_side_plain_when_span_highlighting_is_active() {
        let mut out = String::new();
        let old_line_spans = std::collections::HashMap::new();
        let new_line_spans = std::collections::HashMap::new();
        let columns = std::collections::HashMap::new();
        let mut writer = NumberedDiffWriter {
            out: &mut out,
            width: 1,
            styles: Styles::ansi(),
            hyperlinks: Hyperlinks::new(None, "a.txt", Some(&columns)),
            span_highlighting: true,
            old_line_spans: &old_line_spans,
            new_line_spans: &new_line_spans,
        };

        writer.write_line(1, '-', "abc", Color::Red, SpanSide::Input);

        assert_eq!(out, "\x1b[2m\x1b[31m1\x1b[m abc\n");
    }

    #[test]
    fn numbered_writer_pairs_old_and_new_lines_positionally() {
        let mut out = String::new();
        let old_line_spans = std::collections::HashMap::new();
        let new_line_spans = std::collections::HashMap::new();
        let columns = std::collections::HashMap::new();
        let mut writer = NumberedDiffWriter {
            out: &mut out,
            width: 1,
            styles: Styles::ansi(),
            hyperlinks: Hyperlinks::new(None, "a.txt", Some(&columns)),
            span_highlighting: true,
            old_line_spans: &old_line_spans,
            new_line_spans: &new_line_spans,
        };

        writer.write_block(
            &[(1, "old one"), (2, "old two")],
            &[(1, "new one"), (2, "new two")],
        );

        assert_eq!(
            out,
            "\
\x1b[2m\x1b[31m1\x1b[m old one
\x1b[2m\x1b[32m1\x1b[m new one
\x1b[2m\x1b[31m2\x1b[m old two
\x1b[2m\x1b[32m2\x1b[m new two
"
        );
    }

    #[test]
    fn interactive_inline_diff_helpers_still_highlight_small_token_changes() {
        let inline = inline_token_diff("github.workflow", "githubbworkflow");

        let mut old = String::new();
        write_inline_chars(&mut old, &inline, InlineSide::Old, Styles::ansi());
        assert_eq!(old, "github\x1b[31m\x1b[4m.\x1b[mworkflow");

        let mut new = String::new();
        write_inline_chars(&mut new, &inline, InlineSide::New, Styles::ansi());
        assert_eq!(new, "github\x1b[32m\x1b[4mb\x1b[mworkflow");
    }
}

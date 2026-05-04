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
use crate::theme::{self, Side, StyleSpec};
use crate::ui::Color;
use crate::ui::Styles;

/// Bridge between the local `Color` enum threaded through diff rendering and
/// the configurable per-side palette. Diff code only ever passes `Red` (removed)
/// or `Green` (added); the catch-all arm exists to satisfy exhaustiveness.
const fn side_of(color: Color) -> Side {
    match color {
        Color::Red => Side::Removed,
        Color::Green => Side::Added,
        _ => Side::Added,
    }
}

fn side_diff_style(color: Color) -> StyleSpec {
    let t = theme::theme();
    match color {
        Color::Red => t.style_removed,
        Color::Green => t.style_added,
        _ => StyleSpec::default(),
    }
}

fn side_line_style(color: Color) -> StyleSpec {
    let t = theme::theme();
    match color {
        Color::Red => t.style_line_removed,
        Color::Green => t.style_line_added,
        _ => StyleSpec::default(),
    }
}

#[derive(Clone, Copy)]
pub(crate) struct DiffHints<'a> {
    pub(crate) spans: &'a [Replacement],
    pub(crate) linewise: bool,
    pub(crate) multiline_spans: bool,
}

#[derive(Clone, Copy)]
struct Hyperlinks<'a> {
    template: Option<&'a crate::HyperlinkTemplate<'a>>,
    /// Pre-encoded path used by `{path}`. Empty when the template doesn't
    /// reference `{path}` (caller skips the encoding work in that case).
    encoded_path: &'a str,
    /// 1-indexed line -> first-match column. `None` when not tracked; lookups
    /// for absent lines fall back to column 1 inside `HyperlinkTemplate::render`.
    columns: Option<&'a std::collections::HashMap<usize, usize>>,
}

impl<'a> Hyperlinks<'a> {
    const fn new(
        template: Option<&'a crate::HyperlinkTemplate<'a>>,
        encoded_path: &'a str,
        columns: Option<&'a std::collections::HashMap<usize, usize>>,
    ) -> Self {
        Self {
            template,
            encoded_path,
            columns,
        }
    }

    fn write(self, out: &mut String, line: usize, text: &str) {
        let Some(template) = self.template else {
            out.push_str(text);
            return;
        };
        let column = self
            .columns
            .and_then(|m| m.get(&line).copied())
            .unwrap_or(0);
        out.push_str(&crate::osc8(
            &template.render(self.encoded_path, line, column),
            text,
        ));
    }
}

pub(crate) fn print_file_line_diff(
    old: &str,
    new: &str,
    hints: DiffHints<'_>,
    styles: Styles,
    hyperlink_template: Option<&crate::HyperlinkTemplate<'_>>,
    encoded_path: &str,
    columns: &std::collections::HashMap<usize, usize>,
) {
    let columns = (!columns.is_empty()).then_some(columns);
    let hyperlinks = Hyperlinks::new(hyperlink_template, encoded_path, columns);
    let trimmed: Vec<Replacement> = hints
        .spans
        .iter()
        .map(|s| trim_shared_affixes(*s, old, new))
        .collect();
    let spans = trimmed.as_slice();
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

/// Shrink a span by stripping the longest common prefix and suffix shared by
/// its input and output bytes, so highlighting only covers the actual edit
/// rather than the full matched literal. A literal pattern that ends with `;`
/// being replaced by the same pattern with `;;` should highlight just the
/// trailing punctuation, not the whole expression.
///
/// Always leaves at least one byte on each side: an empty span would be
/// dropped by `group_spans_by_line` and the diff would render with no
/// highlight at all. Bails out for spans that contain a newline (the
/// multi-line and linewise paths assume span endpoints sit at the actual
/// edit boundaries) and for very large spans where the trim scan would
/// dominate per-match cost.
fn trim_shared_affixes(span: Replacement, old: &str, new: &str) -> Replacement {
    const TRIM_AFFIX_LIMIT: usize = 64 * 1024;

    let in_bytes = &old.as_bytes()[span.input_start..span.input_end()];
    let out_bytes = &new.as_bytes()[span.output_start..span.output_end()];
    if in_bytes.is_empty()
        || out_bytes.is_empty()
        || in_bytes.len() > TRIM_AFFIX_LIMIT
        || out_bytes.len() > TRIM_AFFIX_LIMIT
        || in_bytes.contains(&b'\n')
        || out_bytes.contains(&b'\n')
    {
        return span;
    }

    let prefix_max = in_bytes.len().min(out_bytes.len()) - 1;
    let mut prefix = 0;
    while prefix < prefix_max && in_bytes[prefix] == out_bytes[prefix] {
        prefix += 1;
    }
    while prefix > 0
        && (!old.is_char_boundary(span.input_start + prefix)
            || !new.is_char_boundary(span.output_start + prefix))
    {
        prefix -= 1;
    }

    let in_after = &in_bytes[prefix..];
    let out_after = &out_bytes[prefix..];
    let suffix_max = in_after.len().min(out_after.len()) - 1;
    let mut suffix = 0;
    while suffix < suffix_max
        && in_after[in_after.len() - 1 - suffix] == out_after[out_after.len() - 1 - suffix]
    {
        suffix += 1;
    }
    while suffix > 0
        && (!old.is_char_boundary(span.input_end() - suffix)
            || !new.is_char_boundary(span.output_end() - suffix))
    {
        suffix -= 1;
    }

    Replacement {
        input_start: span.input_start + prefix,
        input_len: span.input_len - prefix - suffix,
        output_start: span.output_start + prefix,
        output_len: span.output_len - prefix - suffix,
    }
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
                let style = side_diff_style(Color::Red).without_underline().open(styles);
                let marker = theme::theme().marker_for(Side::Removed, true);
                println!("{style}{marker} {old_line}{}", styles.reset());
                i += 1;
            }
            DiffResult::Right(r) => {
                let style = side_diff_style(Color::Green)
                    .without_underline()
                    .open(styles);
                let marker = theme::theme().marker_for(Side::Added, true);
                println!("{style}{marker} {r}{}", styles.reset());
                i += 1;
            }
        }
    }
}

fn print_inline_diff(old_line: &str, new_line: &str, styles: Styles) {
    let mut out = String::new();
    let inline = inline_token_diff(old_line, new_line);
    let t = theme::theme();
    let _ = write!(
        out,
        "{}{} {}",
        side_diff_style(Color::Red).without_underline().open(styles),
        t.marker_for(Side::Removed, true),
        styles.reset(),
    );
    write_inline_chars(&mut out, &inline, InlineSide::Old, styles);
    out.push('\n');

    let _ = write!(
        out,
        "{}{} {}",
        side_diff_style(Color::Green)
            .without_underline()
            .open(styles),
        t.marker_for(Side::Added, true),
        styles.reset(),
    );
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
            let old_spans = self.old_line_spans.get(&old_line_no);
            let new_spans = self.new_line_spans.get(&new_line_no);
            if self.span_highlighting
                || old_spans.is_some_and(|spans| !spans.is_empty())
                || new_spans.is_some_and(|spans| !spans.is_empty())
            {
                self.write_line_with_spans(old_line_no, old_line, Color::Red, old_spans);
                self.write_line_with_spans(new_line_no, new_line, Color::Green, new_spans);
            } else if should_inline_pair_diff(old_line, new_line) {
                let inline = inline_token_diff(old_line, new_line);
                self.write_inline_line(old_line_no, Color::Red, InlineSide::Old, &inline);
                self.write_inline_line(new_line_no, Color::Green, InlineSide::New, &inline);
            } else {
                self.write_line_with_spans(old_line_no, old_line, Color::Red, old_spans);
                self.write_line_with_spans(new_line_no, new_line, Color::Green, new_spans);
            }
        }
        for (line_no, line) in &old_lines[paired..] {
            self.write_line(*line_no, line, Color::Red, SpanSide::Input);
        }
        for (line_no, line) in &new_lines[paired..] {
            self.write_line(*line_no, line, Color::Green, SpanSide::Output);
        }
    }

    fn write_line(&mut self, line_no: usize, line: &str, color: Color, side: SpanSide) {
        let spans = match side {
            SpanSide::Input => self.old_line_spans.get(&line_no),
            SpanSide::Output => self.new_line_spans.get(&line_no),
        };
        self.write_line_with_spans(line_no, line, color, spans);
    }

    fn write_line_with_spans(
        &mut self,
        line_no: usize,
        line: &str,
        color: Color,
        spans: Option<&Vec<LocalSpan>>,
    ) {
        self.write_prefix(line_no, color);
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
                    side_diff_style(color).without_underline().open(self.styles),
                    self.styles.reset(),
                );
            }
        }
        self.out.push('\n');
    }

    fn write_inline_line(
        &mut self,
        line_no: usize,
        color: Color,
        side: InlineSide,
        inline: &[TokenDiff<'_>],
    ) {
        self.write_prefix(line_no, color);
        write_inline_chars(self.out, inline, side, self.styles);
        self.out.push('\n');
    }

    fn write_prefix(&mut self, line_no: usize, line_color: Color) {
        let line_no_text = line_no.to_string();
        let padding = " ".repeat(self.width.saturating_sub(line_no_text.len()));
        let _ = write!(
            self.out,
            "{}{}",
            side_line_style(line_color)
                .without_underline()
                .open(self.styles),
            padding,
        );
        self.hyperlinks.write(self.out, line_no, &line_no_text);
        self.out.push_str(self.styles.reset());
        let marker = theme::theme().marker_for(side_of(line_color), self.styles.is_plain());
        if marker.is_empty() {
            self.out.push(' ');
        } else {
            let _ = write!(self.out, "{marker} ");
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

const fn should_inline_pair_diff(old_line: &str, new_line: &str) -> bool {
    const MAX_INLINE_PAIR_DIFF_BYTES: usize = 8 * 1024;
    old_line.len() <= MAX_INLINE_PAIR_DIFF_BYTES && new_line.len() <= MAX_INLINE_PAIR_DIFF_BYTES
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
                "{}{own_tok}{}",
                side_diff_style(own_color).open(styles),
                styles.reset(),
            );
        }
    }
}

fn write_underlined_tokens(out: &mut String, tokens: &[&str], color: Color, styles: Styles) {
    for token in tokens {
        let _ = write!(
            out,
            "{}{token}{}",
            side_diff_style(color).open(styles),
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
                    let _ = write!(out, "{}", side_diff_style(color).open(styles));
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
    // order. `apply_compiled_expressions` pushes spans left-to-right, so
    // `input_start` and `output_start` are both monotonically ascending;
    // we walk the buffer once without sorting or collecting.
    let mut line_no: usize = 1;
    let mut line_start: usize = 0;

    for span in spans {
        let (mut start, end) = match side {
            SpanSide::Input => (span.input_start, span.input_end()),
            SpanSide::Output => (span.output_start, span.output_end()),
        };
        if end <= start {
            continue;
        }
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
        let _ = write!(
            out,
            "{}{line}{}",
            side_diff_style(color).without_underline().open(styles),
            styles.reset()
        );
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
            "{}{}{}",
            side_diff_style(color).open(styles),
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
    fn trim_shared_affixes_narrows_to_actual_edit() {
        // "let dir = tempdir().unwrap();" -> same with trailing ";;".
        let old = "let dir = tempdir().unwrap();";
        let new = "let dir = tempdir().unwrap();;";
        let span = rep(0, old.len(), 0, new.len());
        let trimmed = trim_shared_affixes(span, old, new);
        // Common prefix runs up to index 28 (everything but the last `;`),
        // leaving `;` on the input side and `;;` on the output side.
        assert_eq!(trimmed, rep(28, 1, 28, 2));
    }

    #[test]
    fn trim_shared_affixes_keeps_at_least_one_byte_per_side() {
        // Input is a strict prefix of output: trimming naively would empty the
        // input side. Helper must back off to leave one byte on each side.
        let old = "abc";
        let new = "abcd";
        let trimmed = trim_shared_affixes(rep(0, 3, 0, 4), old, new);
        assert_eq!(trimmed, rep(2, 1, 2, 2));
    }

    #[test]
    fn trim_shared_affixes_strips_both_prefix_and_suffix() {
        // Common "ab" prefix and "ef" suffix surrounding the actual edit.
        let old = "abXYef";
        let new = "abQRef";
        let span = rep(0, old.len(), 0, new.len());
        let trimmed = trim_shared_affixes(span, old, new);
        assert_eq!(trimmed, rep(2, 2, 2, 2));
    }

    #[test]
    fn trim_shared_affixes_strips_suffix_only_when_no_common_prefix() {
        // First byte differs, so prefix is empty; suffix "bc" is shared but the
        // helper must leave one byte on each side ("Xb" vs "Yb").
        let old = "Xabc";
        let new = "Yabc";
        let span = rep(0, old.len(), 0, new.len());
        let trimmed = trim_shared_affixes(span, old, new);
        assert_eq!(trimmed, rep(0, 1, 0, 1));
    }

    #[test]
    fn trim_shared_affixes_skips_multiline_spans() {
        let old = "foo\nbar";
        let new = "foo\nbaz";
        let span = rep(0, old.len(), 0, new.len());
        assert_eq!(trim_shared_affixes(span, old, new), span);
    }

    #[test]
    fn trim_shared_affixes_respects_utf8_boundaries() {
        // 'é' is two bytes (0xC3 0xA9) at offsets 3..5. Common prefix counted
        // in bytes would land at offset 4 (mid-codepoint); helper backs off.
        let old = "café!";
        let new = "café?";
        let span = rep(0, old.len(), 0, new.len());
        let trimmed = trim_shared_affixes(span, old, new);
        assert_eq!(trimmed, rep(5, 1, 5, 1));
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
        assert!(!map.contains_key(&3));
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

        writer.write_line(1, "abc", Color::Red, SpanSide::Input);

        assert_eq!(out, "\x1b[31m\x1b[2m1\x1b[m abc\n");
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
\x1b[31m\x1b[2m1\x1b[m old one
\x1b[32m\x1b[2m1\x1b[m new one
\x1b[31m\x1b[2m2\x1b[m old two
\x1b[32m\x1b[2m2\x1b[m new two
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

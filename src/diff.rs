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
    /// Cached `Styles::is_plain()`. Read once at construction so the per-line
    /// emit doesn't re-touch the `OnceLock`-backed color choice on every line.
    plain: bool,
}

impl<'a> Hyperlinks<'a> {
    const fn new(
        template: Option<&'a crate::HyperlinkTemplate<'a>>,
        encoded_path: &'a str,
        columns: Option<&'a std::collections::HashMap<usize, usize>>,
        plain: bool,
    ) -> Self {
        Self {
            template,
            encoded_path,
            columns,
            plain,
        }
    }

    fn write(self, out: &mut String, line: usize, text: &str) {
        let Some(template) = self.template else {
            out.push_str(text);
            return;
        };
        if self.plain {
            out.push_str(text);
            return;
        }
        let column = self
            .columns
            .and_then(|m| m.get(&line).copied())
            .unwrap_or(0);
        out.push_str("\x1b]8;;");
        template.render_into(out, self.encoded_path, line, column);
        out.push_str("\x1b\\");
        out.push_str(text);
        out.push_str("\x1b]8;;\x1b\\");
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn print_file_line_diff<W: std::io::Write>(
    old: &str,
    new: &str,
    hints: DiffHints<'_>,
    styles: Styles,
    hyperlink_template: Option<&crate::HyperlinkTemplate<'_>>,
    encoded_path: &str,
    columns: Option<&std::collections::HashMap<usize, usize>>,
    out: &mut W,
) {
    let hyperlinks = Hyperlinks::new(hyperlink_template, encoded_path, columns, styles.is_plain());
    let mut trimmed: Vec<Replacement> = Vec::with_capacity(hints.spans.len());
    for &span in hints.spans {
        let t = trim_shared_affixes(span, old, new);
        if let Some((left, right)) = split_inner_substring(t, old, new) {
            trimmed.push(left);
            trimmed.push(right);
        } else {
            trimmed.push(t);
        }
    }
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
            out,
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
            out,
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
            out,
        )
    {
        return;
    }

    let diffs = line_diffs(old, new);
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

    // Output is roughly the diff text plus per-line ANSI/OSC-8 overhead. The
    // up-front capacity keeps the per-line `push_str` calls off the realloc
    // path, capped to avoid pathologically large reservations on huge diffs.
    let mut buf = String::with_capacity(
        (old.len() + new.len())
            .saturating_mul(2)
            .min(8 * 1024 * 1024),
    );
    let mut writer = NumberedDiffWriter {
        out: &mut buf,
        width,
        styles,
        hyperlinks,
        span_highlighting,
        old_line_spans: &old_line_spans,
        new_line_spans: &new_line_spans,
        openers: Openers::new(styles),
    };
    for (old_lines, new_lines) in blocks {
        writer.write_block(&old_lines, &new_lines);
    }
    drop(out.write_all(buf.as_bytes()));
}

/// Shrink a span by stripping the longest common prefix and suffix shared by
/// its input and output bytes, so highlighting only covers the actual edit
/// rather than the full matched literal. A literal pattern that ends with `;`
/// being replaced by the same pattern with `;;` should highlight just the
/// trailing punctuation, not the whole expression.
///
/// Allows either side to become empty when the edit is a pure insertion or
/// deletion after trimming shared context, but never trims both sides to
/// empty. Bails out for spans that contain a newline (the multi-line and
/// linewise paths assume span endpoints sit at the actual edit boundaries)
/// and for very large spans where the trim scan would dominate per-match cost.
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

    let prefix_max = in_bytes.len().min(out_bytes.len());
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
    if in_after.is_empty() && out_after.is_empty() {
        return span;
    }

    let suffix_max = in_after.len().min(out_after.len());
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

/// Refines a trimmed span when one side appears verbatim inside the other:
/// splits the larger side into two pure insertion (or deletion) spans wrapping
/// the shared substring, so wrap-style edits like `a` -> `` `a` `` highlight
/// only the inserted delimiters rather than re-marking the preserved content.
///
/// Pure byte-level substring lookup via `memchr::memmem` - no LCS pass and no
/// per-span allocation. `trim_shared_affixes` has already bounded both sides
/// by `TRIM_AFFIX_LIMIT` and aligned them to char boundaries, so a `memmem`
/// match position is also at a char boundary (valid UTF-8 can't contain a
/// multi-byte char's bytes except as that complete char).
fn split_inner_substring(
    span: Replacement,
    old: &str,
    new: &str,
) -> Option<(Replacement, Replacement)> {
    let in_bytes = &old.as_bytes()[span.input_start..span.input_end()];
    let out_bytes = &new.as_bytes()[span.output_start..span.output_end()];

    if in_bytes.is_empty() || out_bytes.is_empty() || in_bytes.len() == out_bytes.len() {
        return None;
    }

    if in_bytes.len() < out_bytes.len() {
        let pos = memchr::memmem::find(out_bytes, in_bytes)?;
        let right_start = pos + in_bytes.len();
        // `trim_shared_affixes` already consumed any shared prefix/suffix, so
        // a flush-to-edge match would leave a zero-length flank - guard so we
        // never emit an invisible span and never re-derive an edge case the
        // edge trim already handled.
        if pos == 0 || right_start == out_bytes.len() {
            return None;
        }
        Some((
            Replacement {
                input_start: span.input_start,
                input_len: 0,
                output_start: span.output_start,
                output_len: pos,
            },
            Replacement {
                input_start: span.input_end(),
                input_len: 0,
                output_start: span.output_start + right_start,
                output_len: out_bytes.len() - right_start,
            },
        ))
    } else {
        let pos = memchr::memmem::find(in_bytes, out_bytes)?;
        let right_start = pos + out_bytes.len();
        if pos == 0 || right_start == in_bytes.len() {
            return None;
        }
        Some((
            Replacement {
                input_start: span.input_start,
                input_len: pos,
                output_start: span.output_start,
                output_len: 0,
            },
            Replacement {
                input_start: span.input_start + right_start,
                input_len: in_bytes.len() - right_start,
                output_start: span.output_end(),
                output_len: 0,
            },
        ))
    }
}

fn replacements_preserve_line_boundaries(old: &str, new: &str, spans: &[Replacement]) -> bool {
    !spans.is_empty()
        && spans.iter().all(|span| {
            (span.input_len > 0 || span.output_len > 0)
                && !old.as_bytes()[span.input_start..span.input_end()].contains(&b'\n')
                && !new.as_bytes()[span.output_start..span.output_end()].contains(&b'\n')
        })
}

fn print_same_line_span_diff<W: std::io::Write>(
    old: &str,
    new: &str,
    styles: Styles,
    hyperlinks: Hyperlinks<'_>,
    old_line_spans: &std::collections::HashMap<usize, Vec<LocalSpan>>,
    new_line_spans: &std::collections::HashMap<usize, Vec<LocalSpan>>,
    out: &mut W,
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
    let mut buf = String::new();
    let mut writer = NumberedDiffWriter {
        out: &mut buf,
        width,
        styles,
        hyperlinks,
        span_highlighting: true,
        old_line_spans,
        new_line_spans,
        openers: Openers::new(styles),
    };

    let mut block_start = 0;
    for idx in 1..=changed_lines.len() {
        if idx == changed_lines.len() || changed_lines[idx] != changed_lines[idx - 1] + 1 {
            writer.write_block(&old_lines[block_start..idx], &new_lines[block_start..idx]);
            block_start = idx;
        }
    }
    drop(out.write_all(buf.as_bytes()));
    true
}

#[allow(clippy::too_many_arguments)]
fn print_linewise_diff<W: std::io::Write>(
    old: &str,
    new: &str,
    styles: Styles,
    hyperlinks: Hyperlinks<'_>,
    span_highlighting: bool,
    old_line_spans: &std::collections::HashMap<usize, Vec<LocalSpan>>,
    new_line_spans: &std::collections::HashMap<usize, Vec<LocalSpan>>,
    out: &mut W,
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
    let mut buf = String::new();
    let mut writer = NumberedDiffWriter {
        out: &mut buf,
        width,
        styles,
        hyperlinks,
        span_highlighting,
        old_line_spans,
        new_line_spans,
        openers: Openers::new(styles),
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
    drop(out.write_all(buf.as_bytes()));
    true
}

#[allow(clippy::too_many_arguments)]
fn print_multiline_span_diff<W: std::io::Write>(
    old: &str,
    new: &str,
    spans: &[Replacement],
    styles: Styles,
    hyperlinks: Hyperlinks<'_>,
    old_line_spans: &std::collections::HashMap<usize, Vec<LocalSpan>>,
    new_line_spans: &std::collections::HashMap<usize, Vec<LocalSpan>>,
    out: &mut W,
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
    let mut buf = String::new();
    let mut writer = NumberedDiffWriter {
        out: &mut buf,
        width,
        styles,
        hyperlinks,
        span_highlighting: true,
        old_line_spans,
        new_line_spans,
        openers: Openers::new(styles),
    };
    for hunk in &mut hunks {
        writer.write_block(&hunk.old_lines, &hunk.new_lines);
    }
    drop(out.write_all(buf.as_bytes()));
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

/// Advance through `text` line by line via a stateful `memchr_iter` cursor,
/// materializing a slice only for the line numbers `line_numbers` actually
/// asks for. Mirrors `str::lines()` semantics: `\n` and `\r\n` are both
/// terminators (and the `\r` is stripped from the line text), a final
/// terminator does not produce an extra empty line. `line_numbers` is
/// expected sorted and deduplicated.
fn lines_for_numbers<'a>(text: &'a str, line_numbers: &[usize]) -> Option<Vec<(usize, &'a str)>> {
    let mut out = Vec::with_capacity(line_numbers.len());
    let mut wanted = line_numbers.iter().copied().peekable();
    let bytes = text.as_bytes();
    if bytes.is_empty() {
        return wanted.peek().is_none().then(Vec::new);
    }
    let mut newlines = memchr::memchr_iter(b'\n', bytes);
    let mut next_nl: Option<usize> = newlines.next();
    let mut start = 0usize;
    let mut line_no = 1usize;
    loop {
        if wanted.peek().is_none() {
            return Some(out);
        }
        let line_end = next_nl.unwrap_or(bytes.len());
        let trim_to = if next_nl.is_some() && line_end > start && bytes[line_end - 1] == b'\r' {
            line_end - 1
        } else {
            line_end
        };
        let line = &text[start..trim_to];
        while wanted.next_if_eq(&line_no).is_some() {
            out.push((line_no, line));
        }
        let Some(nl_pos) = next_nl else {
            return wanted.peek().is_none().then_some(out);
        };
        start = nl_pos + 1;
        line_no += 1;
        if start == bytes.len() {
            return wanted.peek().is_none().then_some(out);
        }
        next_nl = newlines.next();
    }
}

/// Line-level diff producing the `DiffResult` stream the renderers consume.
/// Lines are split on `\n` (a newline-terminated input yields a final empty
/// line, which the renderers skip) and compared with `similar`'s Myers
/// algorithm, which stays near-linear in file size when edits are sparse.
pub(crate) fn line_diffs<'a>(old: &'a str, new: &'a str) -> Vec<DiffResult<&'a str>> {
    use similar::{Algorithm, DiffOp, capture_diff_slices};

    let old_lines: Vec<&str> = old.split('\n').collect();
    let new_lines: Vec<&str> = new.split('\n').collect();
    let mut out = Vec::with_capacity(old_lines.len().max(new_lines.len()));
    for op in capture_diff_slices(Algorithm::Myers, &old_lines, &new_lines) {
        match op {
            DiffOp::Equal {
                old_index,
                new_index,
                len,
            } => out.extend(
                old_lines[old_index..old_index + len]
                    .iter()
                    .zip(&new_lines[new_index..new_index + len])
                    .map(|(o, n)| DiffResult::Both(*o, *n)),
            ),
            DiffOp::Delete {
                old_index, old_len, ..
            } => out.extend(
                old_lines[old_index..old_index + old_len]
                    .iter()
                    .map(|line| DiffResult::Left(*line)),
            ),
            DiffOp::Insert {
                new_index, new_len, ..
            } => out.extend(
                new_lines[new_index..new_index + new_len]
                    .iter()
                    .map(|line| DiffResult::Right(*line)),
            ),
            DiffOp::Replace {
                old_index,
                old_len,
                new_index,
                new_len,
            } => {
                out.extend(
                    old_lines[old_index..old_index + old_len]
                        .iter()
                        .map(|line| DiffResult::Left(*line)),
                );
                out.extend(
                    new_lines[new_index..new_index + new_len]
                        .iter()
                        .map(|line| DiffResult::Right(*line)),
                );
            }
        }
    }
    out
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
                let theme = theme::theme();
                let marker = theme.marker_for(Side::Removed, true);
                let separator = if theme.has_explicit_marker(Side::Removed) {
                    ""
                } else {
                    " "
                };
                println!("{style}{marker}{separator}{old_line}{}", styles.reset());
                i += 1;
            }
            DiffResult::Right(r) => {
                let style = side_diff_style(Color::Green)
                    .without_underline()
                    .open(styles);
                let theme = theme::theme();
                let marker = theme.marker_for(Side::Added, true);
                let separator = if theme.has_explicit_marker(Side::Added) {
                    ""
                } else {
                    " "
                };
                println!("{style}{marker}{separator}{r}{}", styles.reset());
                i += 1;
            }
        }
    }
}

fn print_inline_diff(old_line: &str, new_line: &str, styles: Styles) {
    let mut out = String::new();
    let inline = inline_token_diff(old_line, new_line);
    let t = theme::theme();
    let removed_separator = if t.has_explicit_marker(Side::Removed) {
        ""
    } else {
        " "
    };
    let _ = write!(
        out,
        "{}{}{}{}",
        side_diff_style(Color::Red).without_underline().open(styles),
        t.marker_for(Side::Removed, true),
        removed_separator,
        styles.reset(),
    );
    write_inline_chars(&mut out, &inline, InlineSide::Old, styles);
    out.push('\n');

    let added_separator = if t.has_explicit_marker(Side::Added) {
        ""
    } else {
        " "
    };
    let _ = write!(
        out,
        "{}{}{}{}",
        side_diff_style(Color::Green)
            .without_underline()
            .open(styles),
        t.marker_for(Side::Added, true),
        added_separator,
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

/// Decimal digit count of `n`. `usize::ilog10` for non-zero, falling back to 1
/// for `n == 0` so callers can use this as a width directly. Used to size the
/// padding before the line number without first converting to a `String`.
const fn digit_count(n: usize) -> usize {
    if n == 0 { 1 } else { n.ilog10() as usize + 1 }
}

/// Push `count` spaces into `out` from a fixed `&'static str`. The line-number
/// gutter is always small (single-digit-ish), so a 16-byte source string covers
/// every realistic width without truncation; longer paddings fall back to a
/// loop. Keeps the hot per-line path off the heap.
fn push_spaces(out: &mut String, count: usize) {
    const SPACES: &str = "                ";
    if count <= SPACES.len() {
        out.push_str(&SPACES[..count]);
    } else {
        for _ in 0..count {
            out.push(' ');
        }
    }
}

/// Write `n` as decimal into `buf` and return the matching `&str` slice.
fn format_usize(buf: &mut [u8; 20], mut n: usize) -> &str {
    if n == 0 {
        buf[0] = b'0';
        return std::str::from_utf8(&buf[..1]).expect("digits are ASCII");
    }
    let mut i = buf.len();
    while n > 0 {
        i -= 1;
        buf[i] = b'0' + (n % 10) as u8;
        n /= 10;
    }
    std::str::from_utf8(&buf[i..]).expect("digits are ASCII")
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
    /// Precomputed SGR opening sequences. Resolving the per-side `StyleSpec` and
    /// emitting its SGR codes is invariant for the lifetime of the writer; doing
    /// it once here turns each per-line emit into a single `push_str` of the
    /// cached prefix rather than re-running the style pipeline.
    openers: Openers,
}

/// Precomputed SGR opening sequences for the four colored states the diff
/// writer emits. Built once per writer, used per line.
struct Openers {
    /// `style_line_<side>` with underline stripped - the line-number prefix
    /// color applied to whole lines.
    line_red: String,
    line_green: String,
    /// `style_<side>` with underline stripped - used for the no-span fallback
    /// where the entire line gets the diff color.
    diff_red_no_ul: String,
    diff_green_no_ul: String,
    /// `style_<side>` with underline preserved - wrapped around individual
    /// matched spans inside otherwise-unstyled lines.
    diff_red: String,
    diff_green: String,
}

impl Openers {
    fn new(styles: Styles) -> Self {
        let mut line_red = String::new();
        let mut line_green = String::new();
        let mut diff_red_no_ul = String::new();
        let mut diff_green_no_ul = String::new();
        let mut diff_red = String::new();
        let mut diff_green = String::new();
        side_line_style(Color::Red)
            .without_underline()
            .open_into(&mut line_red, styles);
        side_line_style(Color::Green)
            .without_underline()
            .open_into(&mut line_green, styles);
        side_diff_style(Color::Red)
            .without_underline()
            .open_into(&mut diff_red_no_ul, styles);
        side_diff_style(Color::Green)
            .without_underline()
            .open_into(&mut diff_green_no_ul, styles);
        side_diff_style(Color::Red).open_into(&mut diff_red, styles);
        side_diff_style(Color::Green).open_into(&mut diff_green, styles);
        Self {
            line_red,
            line_green,
            diff_red_no_ul,
            diff_green_no_ul,
            diff_red,
            diff_green,
        }
    }

    fn line_for(&self, color: Color) -> &str {
        match color {
            Color::Red => &self.line_red,
            Color::Green => &self.line_green,
            _ => "",
        }
    }

    fn diff_no_ul_for(&self, color: Color) -> &str {
        match color {
            Color::Red => &self.diff_red_no_ul,
            Color::Green => &self.diff_green_no_ul,
            _ => "",
        }
    }

    fn diff_for(&self, color: Color) -> &str {
        match color {
            Color::Red => &self.diff_red,
            Color::Green => &self.diff_green,
            _ => "",
        }
    }
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
                render_line_with_spans(
                    self.out,
                    line,
                    spans,
                    self.openers.diff_for(color),
                    self.openers.diff_no_ul_for(color),
                    self.styles.reset(),
                );
            }
            _ if self.span_highlighting => {
                self.out.push_str(line);
            }
            _ => {
                self.out.push_str(self.openers.diff_no_ul_for(color));
                self.out.push_str(line);
                self.out.push_str(self.styles.reset());
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
        let line_digits = digit_count(line_no);
        let pad_len = self.width.saturating_sub(line_digits);
        self.out.push_str(self.openers.line_for(line_color));
        push_spaces(self.out, pad_len);
        if self.hyperlinks.template.is_some() && !self.hyperlinks.plain {
            // OSC-8 needs the digits as a `&str` so it can wrap them between
            // the link's open/close escapes. We materialize them on the stack.
            let mut buf = [0u8; 20];
            let line_no_text = format_usize(&mut buf, line_no);
            self.hyperlinks.write(self.out, line_no, line_no_text);
        } else {
            crate::push_decimal(self.out, line_no);
        }
        self.out.push_str(self.styles.reset());
        let theme = theme::theme();
        let side = side_of(line_color);
        let marker = theme.marker_for(side, self.styles.is_plain());
        if theme.has_explicit_marker(side) || self.styles.is_plain() {
            self.out.push_str(marker);
        } else if marker.is_empty() {
            self.out.push(' ');
        } else {
            self.out.push_str(marker);
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

/// Split a line into whitespace runs, single symbols, and words (further
/// split at subword boundaries). Single pass over `char_indices` with a
/// one-char lookahead via `peekable`, so the only allocation is the token
/// vector itself.
pub(crate) fn tokenize(line: &str) -> Vec<&str> {
    let mut tokens = Vec::new();
    let mut iter = line.char_indices().peekable();
    while let Some((start, c)) = iter.next() {
        match classify(c) {
            TokenKind::Symbol => {
                let end = iter.peek().map_or(line.len(), |&(i, _)| i);
                tokens.push(&line[start..end]);
            }
            TokenKind::Whitespace => {
                let mut end = line.len();
                while let Some(&(i, next)) = iter.peek() {
                    if classify(next) != TokenKind::Whitespace {
                        end = i;
                        break;
                    }
                    iter.next();
                }
                tokens.push(&line[start..end]);
            }
            TokenKind::Word => {
                let mut sub_start = start;
                let mut prev = c;
                while let Some(&(i, cur)) = iter.peek() {
                    if classify(cur) != TokenKind::Word {
                        break;
                    }
                    iter.next();
                    // `next` is the raw following char (word or not): the
                    // acronym rule looks one past `cur` regardless of class.
                    let next = iter.peek().map(|&(_, n)| n);
                    if is_subword_boundary(prev, cur, next) {
                        tokens.push(&line[sub_start..i]);
                        sub_start = i;
                    }
                    prev = cur;
                }
                let end = iter.peek().map_or(line.len(), |&(i, _)| i);
                tokens.push(&line[sub_start..end]);
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
    // Walk the buffer's newlines once via a stateful `memchr_iter` cursor.
    // `apply_compiled_expressions` pushes spans left-to-right, so
    // `input_start` and `output_start` are both monotonically ascending;
    // `next_nl` always holds the next `\n` at or after `line_start`, advanced
    // only when we actually step past one. This avoids re-running a fresh
    // `memchr(b'\n', &bytes[line_start..])` on every span iteration.
    let mut newlines = memchr::memchr_iter(b'\n', bytes);
    let mut next_nl: Option<usize> = newlines.next();
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
        while let Some(nl) = next_nl {
            if nl >= start {
                break;
            }
            line_no += 1;
            line_start = nl + 1;
            next_nl = newlines.next();
        }
        // Now `line_start <= start` and either there is no further newline
        // before `start` or it sits past it. Slice the span line by line.
        while start < end {
            let line_end_byte = next_nl.unwrap_or(bytes.len());
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
            debug_assert!(
                next_nl.is_some(),
                "chunk_end < end requires a newline ahead"
            );
            line_no += 1;
            line_start = line_end_byte + 1;
            next_nl = newlines.next();
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
    opener_with_underline: &str,
    opener_without_underline: &str,
    reset: &str,
) {
    if !spans
        .iter()
        .all(|s| line.is_char_boundary(s.start) && line.is_char_boundary(s.end))
    {
        out.push_str(opener_without_underline);
        out.push_str(line);
        out.push_str(reset);
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
        out.push_str(opener_with_underline);
        out.push_str(&line[span.start..span.end]);
        out.push_str(reset);
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
    fn tokenize_splits_words_whitespace_and_symbols() {
        assert_eq!(
            tokenize("let x = 1;"),
            vec!["let", " ", "x", " ", "=", " ", "1", ";"]
        );
        assert_eq!(tokenize("a  b"), vec!["a", "  ", "b"]);
        assert_eq!(tokenize("a+b"), vec!["a", "+", "b"]);
        assert_eq!(tokenize(""), Vec::<&str>::new());
        assert_eq!(tokenize("   "), vec!["   "]);
    }

    #[test]
    fn tokenize_splits_subword_boundaries() {
        assert_eq!(tokenize("fooBar"), vec!["foo", "Bar"]);
        assert_eq!(tokenize("HTTPServer"), vec!["HTTP", "Server"]);
        assert_eq!(tokenize("abc123def"), vec!["abc", "123", "def"]);
        assert_eq!(tokenize("foo_bar"), vec!["foo", "_", "bar"]);
        assert_eq!(tokenize("FOO"), vec!["FOO"]);
    }

    #[test]
    fn tokenize_handles_multibyte_chars() {
        assert_eq!(tokenize("café Bar"), vec!["café", " ", "Bar"]);
        assert_eq!(tokenize("caféBar"), vec!["café", "Bar"]);
        assert_eq!(tokenize("héllo→wörld"), vec!["héllo", "→", "wörld"]);
    }

    #[test]
    fn trim_shared_affixes_narrows_to_actual_edit() {
        // "let dir = tempdir().unwrap();" -> same with trailing ";;".
        let old = "let dir = tempdir().unwrap();";
        let new = "let dir = tempdir().unwrap();;";
        let span = rep(0, old.len(), 0, new.len());
        let trimmed = trim_shared_affixes(span, old, new);
        // Common prefix consumes the old span, leaving only the inserted `;`
        // on the output side.
        assert_eq!(trimmed, rep(old.len(), 0, old.len(), 1));
    }

    #[test]
    fn trim_shared_affixes_allows_one_empty_side() {
        // Input is a strict prefix of output: after trimming shared context,
        // only the inserted byte should remain highlighted.
        let old = "abc";
        let new = "abcd";
        let trimmed = trim_shared_affixes(rep(0, 3, 0, 4), old, new);
        assert_eq!(trimmed, rep(3, 0, 3, 1));
    }

    #[test]
    fn trim_shared_affixes_trims_shared_prefix_that_consumes_one_side() {
        let old = "\"prefix: ";
        let new = "\"";
        let span = rep(0, old.len(), 0, new.len());
        let trimmed = trim_shared_affixes(span, old, new);
        assert_eq!(trimmed, rep(1, old.len() - 1, 1, 0));
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
    fn split_inner_substring_wraps_match_with_inserted_delimiters() {
        // " a " -> " `a` ": after edge trim, "a" vs "`a`". The shared `a` sits
        // in the interior of the new side; split into two insertions flanking
        // it so the diff only highlights the two backticks on the new line.
        let old = " a ";
        let new = " `a` ";
        let trimmed = trim_shared_affixes(rep(0, 3, 0, 5), old, new);
        let (left, right) = split_inner_substring(trimmed, old, new).unwrap();
        assert_eq!(left, rep(1, 0, 1, 1));
        assert_eq!(right, rep(2, 0, 3, 1));
    }

    #[test]
    fn split_inner_substring_handles_deletion_wrapping() {
        // "[abc]" -> "abc": old side is the larger; split into two deletions
        // flanking the preserved `abc`, so only the brackets get highlighted.
        let old = "[abc]";
        let new = "abc";
        let trimmed = trim_shared_affixes(rep(0, 5, 0, 3), old, new);
        let (left, right) = split_inner_substring(trimmed, old, new).unwrap();
        assert_eq!(left, rep(0, 1, 0, 0));
        assert_eq!(right, rep(4, 1, 3, 0));
    }

    #[test]
    fn split_inner_substring_returns_none_when_neither_side_contains_the_other() {
        // After edge trim: "X" vs "Y" - no substring alignment exists.
        let old = "aXc";
        let new = "aYc";
        let trimmed = trim_shared_affixes(rep(0, 3, 0, 3), old, new);
        assert!(split_inner_substring(trimmed, old, new).is_none());
    }

    #[test]
    fn split_inner_substring_returns_none_for_pure_insertion() {
        // "abc" -> "abcd": edge trim collapses input to empty already - the
        // splitter must leave that alone rather than fabricate a flank.
        let old = "abc";
        let new = "abcd";
        let trimmed = trim_shared_affixes(rep(0, 3, 0, 4), old, new);
        assert!(split_inner_substring(trimmed, old, new).is_none());
    }

    #[test]
    fn split_inner_substring_returns_none_when_sides_have_equal_length() {
        // "ab" -> "cd": same length means neither can strictly contain the
        // other; skip without invoking memmem.
        let old = "ab";
        let new = "cd";
        let trimmed = trim_shared_affixes(rep(0, 2, 0, 2), old, new);
        assert!(split_inner_substring(trimmed, old, new).is_none());
    }

    #[test]
    fn split_inner_substring_handles_multibyte_substring() {
        // "é" wraps cleanly with backticks: memmem matches the two-byte UTF-8
        // sequence at offset 1 of "`é`", which is a valid char boundary.
        let old = "é";
        let new = "`é`";
        let trimmed = trim_shared_affixes(rep(0, old.len(), 0, new.len()), old, new);
        let (left, right) = split_inner_substring(trimmed, old, new).unwrap();
        assert_eq!(left, rep(0, 0, 0, 1));
        assert_eq!(right, rep(old.len(), 0, 1 + "é".len(), 1));
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
    fn lines_for_numbers_returns_requested_lines_in_order() {
        let text = "alpha\nbeta\ngamma\ndelta\n";
        let got = lines_for_numbers(text, &[1, 3]);
        assert_eq!(got, Some(vec![(1, "alpha"), (3, "gamma")]));
    }

    #[test]
    fn lines_for_numbers_handles_missing_trailing_newline() {
        let text = "alpha\nbeta\ngamma";
        let got = lines_for_numbers(text, &[1, 2, 3]);
        assert_eq!(got, Some(vec![(1, "alpha"), (2, "beta"), (3, "gamma")]));
    }

    #[test]
    fn lines_for_numbers_strips_crlf_like_str_lines() {
        let text = "alpha\r\nbeta\r\ngamma";
        let got = lines_for_numbers(text, &[1, 2, 3]);
        assert_eq!(got, Some(vec![(1, "alpha"), (2, "beta"), (3, "gamma")]));
    }

    #[test]
    fn lines_for_numbers_returns_none_when_line_is_past_end() {
        let text = "alpha\nbeta";
        let got = lines_for_numbers(text, &[3]);
        assert_eq!(got, None);
    }

    #[test]
    fn lines_for_numbers_treats_empty_text_with_no_requests_as_success() {
        let got = lines_for_numbers("", &[]);
        assert_eq!(got, Some(Vec::new()));
    }

    #[test]
    fn lines_for_numbers_returns_none_for_empty_text_with_any_request() {
        let got = lines_for_numbers("", &[1]);
        assert_eq!(got, None);
    }

    #[test]
    fn lines_for_numbers_does_not_invent_trailing_empty_line() {
        // Mirrors `str::lines()`: "a\n" yields one line, not two. Asking for
        // line 2 must fail even though the buffer ends with a newline.
        let got = lines_for_numbers("alpha\n", &[2]);
        assert_eq!(got, None);
    }

    #[test]
    fn lines_for_numbers_crosses_many_newlines_via_cursor() {
        let text: String = (1..=200).map(|n| format!("L{n}\n")).collect();
        let got = lines_for_numbers(&text, &[1, 50, 100, 200]);
        assert_eq!(
            got,
            Some(vec![(1, "L1"), (50, "L50"), (100, "L100"), (200, "L200")])
        );
    }

    #[test]
    fn lines_for_numbers_preserves_inner_blank_lines() {
        let text = "alpha\n\ngamma\n";
        let got = lines_for_numbers(text, &[1, 2, 3]);
        assert_eq!(got, Some(vec![(1, "alpha"), (2, ""), (3, "gamma")]));
    }

    #[test]
    fn group_spans_by_line_locates_span_on_last_line_without_trailing_newline() {
        // The cursor must surface `next_nl = None` and still slot the span on
        // the final line. Regression for the EOF branch of the body loop.
        let text = "foo\nbar\nbaz";
        let spans = vec![rep(8, 3, 8, 3)];
        let map = group_spans_by_line(text, &spans, SpanSide::Input);
        assert_eq!(map.len(), 1);
        let l3 = &map[&3];
        assert_eq!(l3.len(), 1);
        assert_eq!((l3[0].start, l3[0].end), (0, 3));
    }

    #[test]
    fn group_spans_by_line_handles_many_sequential_spans_across_many_lines() {
        // Many spans spread over many newlines: exercises the shared
        // memchr_iter cursor across the for-loop iterations.
        let text = "0\n1\n2\n3\n4\n5\n6\n7\n8\n9\n";
        let spans = vec![
            rep(0, 1, 0, 1),
            rep(4, 1, 4, 1),
            rep(8, 1, 8, 1),
            rep(12, 1, 12, 1),
            rep(16, 1, 16, 1),
        ];
        let map = group_spans_by_line(text, &spans, SpanSide::Input);
        for (line_no, local_start) in [(1, 0), (3, 0), (5, 0), (7, 0), (9, 0)] {
            let bucket = &map[&line_no];
            assert_eq!(bucket.len(), 1);
            assert_eq!(
                (bucket[0].start, bucket[0].end),
                (local_start, local_start + 1)
            );
        }
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
        let openers = Openers::new(Styles::ansi());
        render_line_with_spans(
            &mut out,
            line,
            &spans,
            openers.diff_for(Color::Red),
            openers.diff_no_ul_for(Color::Red),
            Styles::ansi().reset(),
        );
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
        let openers = Openers::new(Styles::ansi());
        render_line_with_spans(
            &mut out,
            line,
            &spans,
            openers.diff_for(Color::Red),
            openers.diff_no_ul_for(Color::Red),
            Styles::ansi().reset(),
        );
        assert_eq!(out, "\x1b[31mcafé\x1b[m");
    }

    #[test]
    fn render_line_with_spans_with_no_spans_writes_nothing_extra() {
        let line = "unchanged";
        let mut out = String::new();
        let openers = Openers::new(Styles::ansi());
        render_line_with_spans(
            &mut out,
            line,
            &[],
            openers.diff_for(Color::Green),
            openers.diff_no_ul_for(Color::Green),
            Styles::ansi().reset(),
        );
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
            hyperlinks: Hyperlinks::new(None, "a.txt", Some(&columns), false),
            span_highlighting: true,
            old_line_spans: &old_line_spans,
            new_line_spans: &new_line_spans,
            openers: Openers::new(Styles::ansi()),
        };

        writer.write_line(1, "abc", Color::Red, SpanSide::Input);

        assert_eq!(out, "\x1b[31m\x1b[2m1\x1b[m abc\n");
    }

    #[test]
    fn numbered_writer_places_plain_markers_between_line_number_and_text() {
        let mut out = String::new();
        let old_line_spans = std::collections::HashMap::new();
        let new_line_spans = std::collections::HashMap::new();
        let columns = std::collections::HashMap::new();
        let mut writer = NumberedDiffWriter {
            out: &mut out,
            width: 4,
            styles: Styles::PLAIN,
            hyperlinks: Hyperlinks::new(None, "a.txt", Some(&columns), true),
            span_highlighting: true,
            old_line_spans: &old_line_spans,
            new_line_spans: &new_line_spans,
            openers: Openers::new(Styles::PLAIN),
        };

        writer.write_block(&[(1589, "old")], &[(1589, "new")]);

        assert_eq!(out, "1589-old\n1589+new\n");
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
            hyperlinks: Hyperlinks::new(None, "a.txt", Some(&columns), false),
            span_highlighting: true,
            old_line_spans: &old_line_spans,
            new_line_spans: &new_line_spans,
            openers: Openers::new(Styles::ansi()),
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

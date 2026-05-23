// Expression compilation: `-e find=replace` / positional / `-d` /
// `--smart` modes, shared pre-filter matcher, and the counting bulk
// replacers used by the non-interactive apply path.

use std::borrow::Cow;

use anyhow::Context as _;
use anyhow::Result;
use anyhow::bail;
use grep::regex::RegexMatcher;
use grep::regex::RegexMatcherBuilder;
use regex::RegexBuilder;
use regex::bytes::RegexBuilder as BytesRegexBuilder;

use crate::Cli;

/// Internal separator used by `preprocess_expression_args` to join the two
/// space-separated `-e <find> <replace>` args into a single clap value.
/// Null byte is safe because Unix argv strings are null-terminated C strings
/// and can never contain one.
pub(crate) const EXPR_SEP: char = '\x00';

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Expression {
    pub(crate) find: String,
    pub(crate) replace: String,
}

pub(crate) struct CompiledExpression {
    pub(crate) pattern: String,
    pub(crate) regex: regex::Regex,
    pub(crate) bytes_regex: regex::bytes::Regex,
    pub(crate) matcher: RegexMatcher,
    pub(crate) replacer: Box<dyn Fn(&regex::Captures) -> String + Send + Sync>,
    /// Dispatch for `apply_compiled_expressions` - lets each mode use a
    /// `Replacer` impl that appends directly into the destination buffer
    /// instead of allocating a fresh `Vec<u8>` per match.
    pub(crate) bulk: BulkReplacer,
    /// True when this expression cannot add, remove, or match across line
    /// boundaries. Colored diff can then compare old/new lines by number
    /// instead of running an LCS over the whole file.
    pub(crate) preserves_line_boundaries: bool,
}

impl CompiledExpression {
    pub(crate) fn preview_expr(&self) -> crate::interactive::PreviewExpr<'_> {
        crate::interactive::PreviewExpr {
            regex: &self.regex,
            replacer: &*self.replacer,
        }
    }
}

pub(crate) enum BulkReplacer {
    Literal(String),
    Regex(String),
    Smart(std::sync::Arc<std::collections::HashMap<String, String>>),
    Preserve(String),
}

/// Letter-case shape of a matched substring, used by `--preserve` to
/// project the source's casing onto the replacement.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CaseShape {
    Lower,
    Upper,
    Title,
    Mixed,
}

fn detect_case_shape(s: &str) -> CaseShape {
    let mut letters = s.chars().filter(|c| c.is_alphabetic());
    let Some(first) = letters.next() else {
        return CaseShape::Mixed;
    };
    let first_upper = first.is_uppercase();
    let mut has_upper = first_upper;
    let mut has_lower = !first_upper;
    let mut rest_upper = false;
    for c in letters {
        if c.is_uppercase() {
            has_upper = true;
            rest_upper = true;
        } else if c.is_lowercase() {
            has_lower = true;
        }
    }
    if !has_upper {
        CaseShape::Lower
    } else if !has_lower {
        CaseShape::Upper
    } else if first_upper && !rest_upper {
        CaseShape::Title
    } else {
        CaseShape::Mixed
    }
}

fn project_case(source: &str, replacement: &str) -> String {
    match detect_case_shape(source) {
        CaseShape::Lower => replacement.to_lowercase(),
        CaseShape::Upper => replacement.to_uppercase(),
        CaseShape::Title => {
            let mut chars = replacement.chars();
            chars.next().map_or_else(String::new, |first| {
                first
                    .to_uppercase()
                    .chain(chars.flat_map(char::to_lowercase))
                    .collect()
            })
        }
        CaseShape::Mixed => replacement.to_string(),
    }
}

/// Byte-level record of one replacement: the span it consumed in the input
/// and the span it produced in the output. Drives inline highlighting in
/// `print_file_line_diff` directly from the source-of-truth replace operation,
/// avoiding any post-hoc diff guessing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Replacement {
    pub(crate) input_start: usize,
    pub(crate) input_len: usize,
    pub(crate) output_start: usize,
    pub(crate) output_len: usize,
}

impl Replacement {
    pub(crate) const fn input_end(&self) -> usize {
        self.input_start + self.input_len
    }

    pub(crate) const fn output_end(&self) -> usize {
        self.output_start + self.output_len
    }
}

/// Records the input and output spans of a single replacement, when `Some`.
/// `dst_before` is `dst.len()` captured before the replacer wrote the
/// replacement bytes; `dst.len()` after the write gives the output length.
fn record_span(
    spans: &mut Option<&mut Vec<Replacement>>,
    caps: &regex::bytes::Captures<'_>,
    dst_before: usize,
    dst_after: usize,
) {
    if let Some(s) = spans {
        let m = caps.get(0).expect("full match present");
        s.push(Replacement {
            input_start: m.start(),
            input_len: m.end() - m.start(),
            output_start: dst_before,
            output_len: dst_after - dst_before,
        });
    }
}

struct CountingLiteralReplacer<'a> {
    rep: &'a [u8],
    count: usize,
    spans: Option<&'a mut Vec<Replacement>>,
}

impl regex::bytes::Replacer for CountingLiteralReplacer<'_> {
    fn replace_append(&mut self, caps: &regex::bytes::Captures<'_>, dst: &mut Vec<u8>) {
        self.count += 1;
        let before = dst.len();
        dst.extend_from_slice(self.rep);
        record_span(&mut self.spans, caps, before, dst.len());
    }
}

struct CountingRegexReplacer<'a> {
    subst: &'a [u8],
    count: usize,
    spans: Option<&'a mut Vec<Replacement>>,
}

impl regex::bytes::Replacer for CountingRegexReplacer<'_> {
    fn replace_append(&mut self, caps: &regex::bytes::Captures<'_>, dst: &mut Vec<u8>) {
        self.count += 1;
        let before = dst.len();
        caps.expand(self.subst, dst);
        record_span(&mut self.spans, caps, before, dst.len());
    }
}

struct CountingPreserveReplacer<'a> {
    replacement: &'a str,
    count: usize,
    spans: Option<&'a mut Vec<Replacement>>,
}

impl regex::bytes::Replacer for CountingPreserveReplacer<'_> {
    fn replace_append(&mut self, caps: &regex::bytes::Captures<'_>, dst: &mut Vec<u8>) {
        self.count += 1;
        let before = dst.len();
        let matched = caps.get(0).expect("full regex match is always present");
        // The pattern compiled for `--preserve` is `regex::escape`d from a
        // user-supplied UTF-8 string, so every match is a UTF-8 substring of
        // the haystack. A non-UTF-8 substring could never have matched.
        let source = std::str::from_utf8(matched.as_bytes())
            .expect("preserve pattern matches are always UTF-8");
        dst.extend_from_slice(project_case(source, self.replacement).as_bytes());
        record_span(&mut self.spans, caps, before, dst.len());
    }
}

struct CountingSmartReplacer<'a> {
    map: &'a std::collections::HashMap<String, String>,
    count: usize,
    spans: Option<&'a mut Vec<Replacement>>,
}

impl regex::bytes::Replacer for CountingSmartReplacer<'_> {
    fn replace_append(&mut self, caps: &regex::bytes::Captures<'_>, dst: &mut Vec<u8>) {
        self.count += 1;
        let before = dst.len();
        let matched = caps.get(0).expect("full regex match is always present");
        // Smart-replace patterns are built from case conversions of
        // the user's find string. Those are always valid UTF-8, so every
        // match here is a UTF-8 substring of the haystack - the `from_utf8`
        // always succeeds. A non-UTF-8 substring could never have matched.
        let key = std::str::from_utf8(matched.as_bytes())
            .expect("smart pattern alternatives are always UTF-8");
        dst.extend_from_slice(
            self.map
                .get(key)
                .expect("smart replacer map must contain every regex alternative")
                .as_bytes(),
        );
        record_span(&mut self.spans, caps, before, dst.len());
    }
}

/// Build the 7 case variant pairs for preserve-case replacement.
/// Returns (`variant_map`, `regex_pattern`).
pub(crate) fn build_case_variants(
    pattern: &str,
    replacement: &str,
) -> (std::collections::HashMap<String, String>, String) {
    use convert_case::{Case, Casing as _};

    fn to_ada_case(input: &str) -> String {
        input.to_case(Case::Ada)
    }

    fn to_camel_case(input: &str) -> String {
        input.to_case(Case::Camel)
    }

    fn to_kebab_case(input: &str) -> String {
        input.to_case(Case::Kebab)
    }

    fn to_pascal_case(input: &str) -> String {
        input.to_case(Case::Pascal)
    }

    fn to_screaming_snake_case(input: &str) -> String {
        input.to_case(Case::UpperSnake)
    }

    fn to_snake_case(input: &str) -> String {
        input.to_case(Case::Snake)
    }

    fn to_train_case(input: &str) -> String {
        input.to_case(Case::Train)
    }

    fn normalize_separators(input: &str) -> String {
        input.replace(['_', '-'], " ")
    }

    let converters: &[fn(&str) -> String] = &[
        to_ada_case,
        to_camel_case,
        to_kebab_case,
        to_pascal_case,
        to_screaming_snake_case,
        to_snake_case,
        to_train_case,
    ];

    let mut map = std::collections::HashMap::new();
    let mut alt_parts = Vec::new();
    let seeds = [
        (pattern.to_string(), replacement.to_string()),
        (
            normalize_separators(pattern),
            normalize_separators(replacement),
        ),
    ];

    for (pattern_seed, replacement_seed) in seeds {
        for convert in converters {
            let from = convert(&pattern_seed);
            let to = convert(&replacement_seed);
            if !from.is_empty() && !map.contains_key(&from) {
                alt_parts.push(regex::escape(&from));
                map.insert(from, to);
            }
        }
    }

    // Sort longest first so regex alternation matches greedily
    alt_parts.sort_by_key(|a| std::cmp::Reverse(a.len()));
    let regex_pattern = alt_parts.join("|");

    (map, regex_pattern)
}

pub(crate) fn build_pattern_for(cli: &Cli, pattern: &str) -> String {
    let base = if cli.is_regex() {
        pattern.to_string()
    } else {
        regex::escape(pattern)
    };

    let wrapped = if cli.line_regexp {
        format!("^(?:{base})$")
    } else if cli.word_regexp {
        format!(r"\b(?:{base})\b")
    } else {
        base
    };

    let inner = if cli.is_regex() && !cli.greedy {
        format!("(?U){wrapped}")
    } else {
        wrapped
    };

    if cli.delete {
        wrap_delete_pattern(&inner, cli.line_regexp)
    } else {
        inner
    }
}

/// Extend a match pattern to consume the full line(s) it sits on, plus any
/// single trailing newline, so an empty replacement removes whole lines.
///
/// The user's pattern is kept inside a non-capturing group so an embedded
/// `(?U)` inverted-greediness flag stays scoped - otherwise the wrapper's
/// `[^\n]*` runs would flip to non-greedy and leave a tail of the line.
fn wrap_delete_pattern(inner: &str, line_regexp: bool) -> String {
    if line_regexp {
        // `inner` already anchors `^...$` for whole-line matches.
        format!(r"(?:{inner})\n?")
    } else {
        format!(r"^[^\n]*(?:{inner})[^\n]*\n?")
    }
}

pub(crate) fn build_subst_for(cli: &Cli, replacement: &str) -> String {
    if cli.is_regex() {
        replacement.to_string()
    } else {
        replacement.replace('$', "$$")
    }
}

fn parse_expression(input: &str) -> Result<Expression> {
    let Some((find, replace)) = input.split_once(EXPR_SEP) else {
        bail!("Invalid expression: expected `-e <find> <replace>`");
    };
    Ok(Expression {
        find: find.to_string(),
        replace: replace.to_string(),
    })
}

fn parse_expressions(cli: &Cli) -> Result<Vec<Expression>> {
    cli.expressions
        .iter()
        .map(|expr| parse_expression(expr))
        .collect()
}

fn compile_expression(cli: &Cli, expr: &Expression) -> Result<CompiledExpression> {
    if cli.preserve {
        // Literal pattern, case-insensitive via embedded `(?i:...)` so the
        // case-folding stays scoped when this pattern is later joined into
        // an alternation by `build_pre_filter_matcher`.
        let escaped = format!("(?i:{})", regex::escape(&expr.find));
        let wrapped = if cli.line_regexp {
            format!("^(?:{escaped})$")
        } else if cli.word_regexp {
            format!(r"\b(?:{escaped})\b")
        } else {
            escaped
        };
        let pattern = if cli.delete {
            wrap_delete_pattern(&wrapped, cli.line_regexp)
        } else {
            wrapped
        };
        let regex = RegexBuilder::new(&pattern)
            .multi_line(true)
            .build()
            .with_context(|| format!("Invalid pattern: {}", expr.find))?;
        let bytes_regex = BytesRegexBuilder::new(&pattern)
            .multi_line(true)
            .build()
            .with_context(|| format!("Invalid pattern: {}", expr.find))?;
        let matcher = RegexMatcherBuilder::new()
            .multi_line(true)
            .build(&pattern)?;
        if cli.delete {
            return Ok(CompiledExpression {
                pattern,
                regex,
                bytes_regex,
                matcher,
                replacer: Box::new(|_: &regex::Captures| String::new()),
                bulk: BulkReplacer::Literal(String::new()),
                preserves_line_boundaries: false,
            });
        }
        let replacement = expr.replace.clone();
        let closure_replacement = replacement.clone();
        let replacer = move |caps: &regex::Captures| -> String {
            let matched = caps
                .get(0)
                .expect("full regex match is always present")
                .as_str();
            project_case(matched, &closure_replacement)
        };
        return Ok(CompiledExpression {
            pattern,
            regex,
            bytes_regex,
            matcher,
            replacer: Box::new(replacer),
            bulk: BulkReplacer::Preserve(replacement),
            preserves_line_boundaries: !expr.find.contains('\n') && !expr.replace.contains('\n'),
        });
    }
    if cli.smart {
        let (variant_map, variant_pattern) = build_case_variants(&expr.find, &expr.replace);
        let wrapped = if cli.line_regexp {
            format!("^(?:{variant_pattern})$")
        } else if cli.word_regexp {
            format!(r"\b(?:{variant_pattern})\b")
        } else {
            variant_pattern
        };
        // With `-d --smart`, the case-variant alternation becomes the "inner" of
        // a line-deleting wrapper, and the replacement is always empty.
        let pattern = if cli.delete {
            wrap_delete_pattern(&wrapped, cli.line_regexp)
        } else {
            wrapped
        };
        let regex = RegexBuilder::new(&pattern)
            .multi_line(true)
            .build()
            .with_context(|| format!("Invalid smart pattern: {}", expr.find))?;
        let bytes_regex = BytesRegexBuilder::new(&pattern)
            .multi_line(true)
            .build()
            .with_context(|| format!("Invalid smart pattern: {}", expr.find))?;
        let matcher = RegexMatcherBuilder::new()
            .multi_line(true)
            .build(&pattern)?;
        if cli.delete {
            return Ok(CompiledExpression {
                pattern,
                regex,
                bytes_regex,
                matcher,
                replacer: Box::new(|_: &regex::Captures| String::new()),
                bulk: BulkReplacer::Literal(String::new()),
                preserves_line_boundaries: false,
            });
        }
        let variant_map = std::sync::Arc::new(variant_map);
        let closure_map = std::sync::Arc::clone(&variant_map);
        let replacer = move |caps: &regex::Captures| -> String {
            let matched = caps
                .get(0)
                .expect("full regex match is always present")
                .as_str();
            closure_map
                .get(matched)
                .cloned()
                .expect("smart replacer map must contain every regex alternative")
        };
        Ok(CompiledExpression {
            pattern,
            regex,
            bytes_regex,
            matcher,
            replacer: Box::new(replacer),
            bulk: BulkReplacer::Smart(variant_map),
            preserves_line_boundaries: !expr.find.contains('\n') && !expr.replace.contains('\n'),
        })
    } else {
        let pattern = build_pattern_for(cli, &expr.find);
        let subst = build_subst_for(cli, &expr.replace);
        let dot_matches_new_line = cli.dotall || cli.multiline;
        let regex = RegexBuilder::new(&pattern)
            .case_insensitive(cli.ignore_case)
            .multi_line(true)
            .dot_matches_new_line(dot_matches_new_line)
            .build()
            .with_context(|| format!("Invalid regex: {}", expr.find))?;
        let bytes_regex = BytesRegexBuilder::new(&pattern)
            .case_insensitive(cli.ignore_case)
            .multi_line(true)
            .dot_matches_new_line(dot_matches_new_line)
            .build()
            .with_context(|| format!("Invalid regex: {}", expr.find))?;
        let matcher = RegexMatcherBuilder::new()
            .case_insensitive(cli.ignore_case)
            .multi_line(true)
            .dot_matches_new_line(dot_matches_new_line)
            .build(&pattern)?;
        let bulk = if cli.is_regex() {
            BulkReplacer::Regex(subst.clone())
        } else {
            BulkReplacer::Literal(expr.replace.clone())
        };
        let replacer = move |caps: &regex::Captures| -> String {
            let mut out = String::with_capacity(subst.len());
            caps.expand(&subst, &mut out);
            out
        };
        Ok(CompiledExpression {
            pattern,
            regex,
            bytes_regex,
            matcher,
            replacer: Box::new(replacer),
            bulk,
            preserves_line_boundaries: !cli.is_regex()
                && !cli.delete
                && !expr.find.contains('\n')
                && !expr.replace.contains('\n'),
        })
    }
}

pub(crate) fn build_pre_filter_matcher(
    cli: &Cli,
    expressions: &[CompiledExpression],
) -> Result<RegexMatcher> {
    if expressions.len() == 1 {
        return Ok(expressions[0].matcher.clone());
    }
    let union = expressions
        .iter()
        .map(|e| format!("(?:{})", e.pattern))
        .collect::<Vec<_>>()
        .join("|");
    let mut builder = RegexMatcherBuilder::new();
    builder
        .multi_line(true)
        .dot_matches_new_line(cli.dotall || cli.multiline);
    if !cli.smart {
        builder.case_insensitive(cli.ignore_case);
    }
    builder
        .build(&union)
        .with_context(|| format!("Invalid union pre-filter pattern: {union}"))
}

pub(crate) fn compile_expressions(cli: &Cli) -> Result<Vec<CompiledExpression>> {
    let expressions = if cli.uses_expressions() {
        if cli.delete {
            // In delete mode the replace half of `-e <find> <replace>` is
            // ignored. Extract only the find part (before the \x00 separator).
            cli.expressions
                .iter()
                .map(|raw| Expression {
                    find: raw
                        .split_once(EXPR_SEP)
                        .map_or(raw.as_str(), |(f, _)| f)
                        .to_string(),
                    replace: String::new(),
                })
                .collect()
        } else {
            parse_expressions(cli)?
        }
    } else if cli.args.is_empty() {
        // Reachable only under `-l` with no positional `<find>` (the main
        // entry guard requires args otherwise). An empty expression set means
        // "no content filter" - the walker output is the listing as-is.
        Vec::new()
    } else {
        vec![Expression {
            find: cli.pattern().to_string(),
            replace: cli
                .positional_replace()
                .map(str::to_string)
                .unwrap_or_default(),
        }]
    };

    expressions
        .iter()
        .filter(|expr| cli.is_regex() || cli.smart || expr.find != expr.replace)
        .map(|expr| compile_expression(cli, expr))
        .collect()
}

/// Applies the chain of expressions and returns:
///   - the rewritten bytes,
///   - the total replacement count,
///   - byte offsets of matches in the *original* input from the **first**
///     expression only (empty when `track_positions` is false, or when the
///     first expression has no matches).
///
/// Position tracking is gated by the caller because it costs one Vec push
/// per match. Callers that don't render diffs (file writes to disk, piped
/// output, stdin mode) pass `false` and pay nothing.
///
/// Only the first expression's positions are tracked: later expressions
/// match against the rewritten text, so their offsets wouldn't index into
/// either the displayed `-` (original) or `+` (final) lines correctly.
pub(crate) fn apply_compiled_expressions<'a>(
    contents: &'a [u8],
    expressions: &[CompiledExpression],
    track_spans: bool,
) -> (Cow<'a, [u8]>, usize, Vec<Replacement>) {
    use regex::bytes::Replacer as _;
    let mut current: Cow<'a, [u8]> = Cow::Borrowed(contents);
    let mut replacements = 0;
    let mut spans: Vec<Replacement> = Vec::new();

    for (idx, expr) in expressions.iter().enumerate() {
        // Spans are captured only against the original input (`idx == 0`).
        // Later expressions operate on the previous expression's output, so
        // their spans are not directly meaningful against the final buffer.
        // Callers that need fully accurate inline highlighting check
        // `expressions.len() == 1` before consuming spans for that purpose.
        let spans_ref = (track_spans && idx == 0).then_some(&mut spans);

        let (replaced, count) = match &expr.bulk {
            BulkReplacer::Literal(rep) => {
                let mut rep = CountingLiteralReplacer {
                    rep: rep.as_bytes(),
                    count: 0,
                    spans: spans_ref,
                };
                let out = expr.bytes_regex.replace_all(&current, rep.by_ref());
                (out, rep.count)
            }
            BulkReplacer::Regex(subst) => {
                let mut rep = CountingRegexReplacer {
                    subst: subst.as_bytes(),
                    count: 0,
                    spans: spans_ref,
                };
                let out = expr.bytes_regex.replace_all(&current, rep.by_ref());
                (out, rep.count)
            }
            BulkReplacer::Smart(map) => {
                let mut rep = CountingSmartReplacer {
                    map,
                    count: 0,
                    spans: spans_ref,
                };
                let out = expr.bytes_regex.replace_all(&current, rep.by_ref());
                (out, rep.count)
            }
            BulkReplacer::Preserve(replacement) => {
                let mut rep = CountingPreserveReplacer {
                    replacement,
                    count: 0,
                    spans: spans_ref,
                };
                let out = expr.bytes_regex.replace_all(&current, rep.by_ref());
                (out, rep.count)
            }
        };
        if count > 0 {
            replacements += count;
            current = Cow::Owned(replaced.into_owned());
        }
    }

    (current, replacements, spans)
}

/// Builds the per-line first-column map only when the caller's hyperlink
/// format string actually consumes `{column}`. Returns `None` otherwise,
/// skipping the input scan and the per-file `HashMap` construction entirely.
/// This is the pay-for-what-you-use gate that keeps replacement runs cheap
/// when the format omits `{column}`.
pub(crate) fn first_column_map_if_needed(
    needs_first_column: bool,
    input: &[u8],
    spans: &[Replacement],
) -> Option<std::collections::HashMap<usize, usize>> {
    if !needs_first_column {
        return None;
    }
    let input_starts: Vec<usize> = spans.iter().map(|s| s.input_start).collect();
    Some(byte_offsets_to_line_first_column(input, &input_starts))
}

/// Walks `input` once, mapping a sorted slice of byte offsets to the
/// 1-indexed `(line, column)` of the first match on each line. Single linear
/// pass, `O(input.len() + offsets.len())`, using a stateful `memchr_iter`
/// cursor so each newline crossing pulls one position from the iterator
/// rather than re-running `memchr` on a fresh sub-slice.
pub(crate) fn byte_offsets_to_line_first_column(
    input: &[u8],
    offsets: &[usize],
) -> std::collections::HashMap<usize, usize> {
    use std::collections::HashMap;
    let mut map: HashMap<usize, usize> = HashMap::new();
    if offsets.is_empty() {
        return map;
    }

    let mut sorted = offsets.to_vec();
    sorted.sort_unstable();
    let mut newlines = memchr::memchr_iter(b'\n', input);
    let mut next_nl: Option<usize> = newlines.next();
    let mut line: usize = 1;
    let mut line_start: usize = 0;

    for &off in &sorted {
        // Advance past newlines that end before this offset.
        while let Some(nl) = next_nl {
            if nl >= off {
                break;
            }
            line += 1;
            line_start = nl + 1;
            next_nl = newlines.next();
        }
        let col = off.saturating_sub(line_start) + 1;
        map.entry(line).or_insert(col);
    }
    map
}

#[cfg(test)]
mod tests {
    use clap::Parser as _;

    use super::*;

    fn parse_cli(args: &[&str]) -> Cli {
        let _lock = crate::test_env::lock_for_parse();
        let processed = crate::preprocess_expression_args(
            args.iter().map(std::string::ToString::to_string).collect(),
        );
        Cli::parse_from(processed)
    }

    fn build_pattern(cli: &Cli) -> String {
        build_pattern_for(cli, cli.pattern())
    }

    fn build_subst(cli: &Cli) -> String {
        build_subst_for(cli, cli.replacement())
    }

    #[test]
    fn test_byte_offsets_to_line_first_column_basic() {
        // input:   "abc\ndefoo\nfoox"
        //          line 1: abc            (cols 1..3)
        //          line 2: defoo          (cols 1..5,  newline at byte 9)
        //          line 3: foox           (cols 1..4)
        // matches at byte offsets 6 (line 2, col 3) and 10 (line 3, col 1)
        let input = b"abc\ndefoo\nfoox";
        let map = byte_offsets_to_line_first_column(input, &[6, 10]);
        assert_eq!(map.get(&2), Some(&3));
        assert_eq!(map.get(&3), Some(&1));
        assert_eq!(map.get(&1), None);
    }

    #[test]
    fn test_byte_offsets_to_line_first_column_records_first_only() {
        // Two matches on the same line: only the earliest column is kept.
        let input = b"foofoo\n";
        let map = byte_offsets_to_line_first_column(input, &[3, 0]);
        assert_eq!(map.get(&1), Some(&1));
    }

    #[test]
    fn test_byte_offsets_to_line_first_column_empty_offsets() {
        let map = byte_offsets_to_line_first_column(b"abc\ndef\n", &[]);
        assert!(map.is_empty());
    }

    #[test]
    fn test_byte_offsets_to_line_first_column_crosses_many_newlines() {
        // Exercises the shared memchr_iter cursor across many advances:
        // 10 single-char lines, one offset per line at column 1.
        let input = b"a\nb\nc\nd\ne\nf\ng\nh\ni\nj\n";
        let offsets = [0, 2, 4, 6, 8, 10, 12, 14, 16, 18];
        let map = byte_offsets_to_line_first_column(input, &offsets);
        let mut got: Vec<(usize, usize)> = map.into_iter().collect();
        got.sort_unstable();
        let want: Vec<(usize, usize)> = (1..=10).map(|n| (n, 1)).collect();
        assert_eq!(got, want);
    }

    #[test]
    fn test_first_column_map_skips_when_not_needed() {
        // No allocation when the caller signals `{column}` isn't in the format.
        let spans = [Replacement {
            input_start: 6,
            input_len: 3,
            output_start: 6,
            output_len: 3,
        }];
        let map = first_column_map_if_needed(false, b"abc\ndefoo\n", &spans);
        assert!(map.is_none());
    }

    #[test]
    fn test_first_column_map_computes_when_needed() {
        let spans = [Replacement {
            input_start: 6,
            input_len: 3,
            output_start: 6,
            output_len: 3,
        }];
        let map = first_column_map_if_needed(true, b"abc\ndefoo\n", &spans);
        assert_eq!(map.unwrap().get(&2), Some(&3));
    }

    #[test]
    fn test_first_column_map_empty_spans_when_needed() {
        // Needed but no spans -> Some(empty); the gate is still on.
        let map = first_column_map_if_needed(true, b"abc\ndef\n", &[]);
        assert!(map.expect("needed branch returns Some").is_empty());
    }

    #[test]
    fn test_apply_compiled_expressions_returns_spans_when_tracking_enabled() {
        let cli = parse_cli(&["rep", "foo", "bar"]);
        let expressions = compile_expressions(&cli).unwrap();
        // "foo" appears at byte offsets 0 and 8 in "foo bar\nfoo baz\n".
        // Each match is 3 bytes ("foo") and produces 3 bytes ("bar"), so
        // input/output offsets line up.
        let (_out, count, spans) =
            apply_compiled_expressions(b"foo bar\nfoo baz\n", &expressions, true);
        assert_eq!(count, 2);
        assert_eq!(
            spans,
            vec![
                Replacement {
                    input_start: 0,
                    input_len: 3,
                    output_start: 0,
                    output_len: 3,
                },
                Replacement {
                    input_start: 8,
                    input_len: 3,
                    output_start: 8,
                    output_len: 3,
                },
            ]
        );
    }

    #[test]
    fn test_apply_compiled_expressions_skips_spans_when_tracking_disabled() {
        let cli = parse_cli(&["rep", "foo", "bar"]);
        let expressions = compile_expressions(&cli).unwrap();
        let (_out, count, spans) =
            apply_compiled_expressions(b"foo bar\nfoo baz\n", &expressions, false);
        assert_eq!(count, 2);
        assert!(spans.is_empty());
    }

    #[test]
    fn test_apply_compiled_expressions_tracks_output_offsets_when_lengths_differ() {
        // Replacement is longer than the match, so output offsets shift past
        // the second match relative to input offsets.
        let cli = parse_cli(&["rep", "ab", "xyz"]);
        let expressions = compile_expressions(&cli).unwrap();
        let (_out, count, spans) = apply_compiled_expressions(b"ab cd ab", &expressions, true);
        assert_eq!(count, 2);
        assert_eq!(
            spans,
            vec![
                Replacement {
                    input_start: 0,
                    input_len: 2,
                    output_start: 0,
                    output_len: 3,
                },
                Replacement {
                    input_start: 6,
                    input_len: 2,
                    output_start: 7,
                    output_len: 3,
                },
            ]
        );
    }

    fn apply_str<'a>(
        contents: &'a str,
        expressions: &[CompiledExpression],
    ) -> (Cow<'a, str>, usize) {
        let (out, n, _) = apply_compiled_expressions(contents.as_bytes(), expressions, false);
        let cow = match out {
            Cow::Borrowed(b) => Cow::Borrowed(std::str::from_utf8(b).unwrap()),
            Cow::Owned(o) => Cow::Owned(String::from_utf8(o).unwrap()),
        };
        (cow, n)
    }

    #[test]
    fn test_build_case_variants() {
        let (map, pattern) = build_case_variants("foo_bar", "spam_eggs");

        assert_eq!(map.get("foo_bar"), Some(&"spam_eggs".to_string()));
        assert_eq!(map.get("FooBar"), Some(&"SpamEggs".to_string()));
        assert_eq!(map.get("FOO_BAR"), Some(&"SPAM_EGGS".to_string()));
        assert_eq!(map.get("foo-bar"), Some(&"spam-eggs".to_string()));
        assert_eq!(map.get("fooBar"), Some(&"spamEggs".to_string()));
        assert_eq!(map.get("Foo-Bar"), Some(&"Spam-Eggs".to_string())); // train-case
        assert_eq!(map.get("Foo_Bar"), Some(&"Spam_Eggs".to_string())); // ada_case

        let regex = regex::Regex::new(&pattern).unwrap();
        assert!(regex.is_match("foo_bar"));
        assert!(regex.is_match("FooBar"));
        assert!(regex.is_match("FOO_BAR"));
        assert!(regex.is_match("foo-bar"));
        assert!(regex.is_match("fooBar"));
        assert!(!regex.is_match("foobar"));
    }

    #[test]
    fn test_build_case_variants_replacement() {
        let (map, pattern) = build_case_variants("foo_bar", "spam_eggs");
        let regex = regex::Regex::new(&pattern).unwrap();

        let input = "let foo_bar = FooBar::new(FOO_BAR);";
        let output = regex.replace_all(input, |caps: &regex::Captures| {
            map.get(
                caps.get(0)
                    .expect("full regex match is always present")
                    .as_str(),
            )
            .cloned()
            .expect("smart replacer map must contain every regex alternative")
        });
        assert_eq!(output, "let spam_eggs = SpamEggs::new(SPAM_EGGS);");
    }

    #[test]
    fn test_parse_expression_splits_on_null_byte() {
        assert_eq!(
            parse_expression(&format!("a{EXPR_SEP}b=c")).unwrap(),
            Expression {
                find: "a".to_string(),
                replace: "b=c".to_string(),
            }
        );
    }

    #[test]
    fn test_compile_expressions_applies_in_order() {
        let cli = parse_cli(&["rep", "-e", "a", "b", "-e", "b", "c", "src"]);

        let expressions = compile_expressions(&cli).unwrap();
        let (output, count) = apply_str("a b", &expressions);
        assert_eq!(output, "c c");
        assert_eq!(count, 3);
    }

    /// Regression: the preview-mode replacer was building `new_contents` as
    /// `contents[..offset] + repl + contents[offset+mat.end()..]`, dropping
    /// `contents[offset..offset+mat.start()]` - the text between the search
    /// window and the actual match position.
    #[test]
    fn test_expression_preserves_text_before_match() {
        let cli = parse_cli(&["rep", "-e", "a", "b"]);
        let expressions = compile_expressions(&cli).unwrap();
        let (output, count) = apply_str("#![allow(clippy::all)]", &expressions);
        assert_eq!(output, "#![bllow(clippy::bll)]");
        assert_eq!(count, 2);
    }

    #[test]
    fn test_parse_expression_missing_null_byte() {
        assert!(parse_expression("no-null-here").is_err());
    }

    #[test]
    fn test_apply_compiled_expressions_no_matches() {
        let cli = parse_cli(&["rep", "-e", "xyz", "abc"]);
        let expressions = compile_expressions(&cli).unwrap();
        let (output, count) = apply_str("hello world", &expressions);
        assert_eq!(output, "hello world");
        assert_eq!(count, 0);
    }

    #[test]
    fn test_build_subst_escapes_dollar_in_literal_mode() {
        let cli = parse_cli(&["rep", "foo", "$1"]);
        assert_eq!(build_subst(&cli), "$$1");
    }

    #[test]
    fn test_build_subst_preserves_dollar_in_regex_mode() {
        let cli = parse_cli(&["rep", "-r", "(foo)", "$1"]);
        assert_eq!(build_subst(&cli), "$1");
    }

    #[test]
    fn test_build_pattern_escapes_metacharacters() {
        let cli = parse_cli(&["rep", "1.2.3", "4.5.6"]);
        assert_eq!(build_pattern(&cli), r"1\.2\.3");
    }

    #[test]
    fn test_build_pattern_regex_non_greedy_by_default() {
        let cli = parse_cli(&["rep", "-r", "a.*b", "x"]);
        assert_eq!(build_pattern(&cli), "(?U)a.*b");
    }

    #[test]
    fn test_build_pattern_regex_greedy() {
        let cli = parse_cli(&["rep", "-r", "-G", "a.*b", "x"]);
        assert_eq!(build_pattern(&cli), "a.*b");
    }

    #[test]
    fn test_build_pattern_word_boundary() {
        let cli = parse_cli(&["rep", "-w", "foo", "bar"]);
        assert_eq!(build_pattern(&cli), r"(?U)\b(?:foo)\b");
    }

    #[test]
    fn test_build_pattern_line_regexp() {
        let cli = parse_cli(&["rep", "-x", "foo", "bar"]);
        assert_eq!(build_pattern(&cli), "(?U)^(?:foo)$");
    }

    #[test]
    fn test_detect_case_shape_classifies_each_shape() {
        assert_eq!(detect_case_shape("colour"), CaseShape::Lower);
        assert_eq!(detect_case_shape("Colour"), CaseShape::Title);
        assert_eq!(detect_case_shape("COLOUR"), CaseShape::Upper);
        assert_eq!(detect_case_shape("cOlOuR"), CaseShape::Mixed);
        assert_eq!(detect_case_shape("CoLoUr"), CaseShape::Mixed);
        // Single-letter sources: one upper char is unambiguously Upper, one
        // lower char is Lower. Both project to the same string regardless,
        // but the classification still has to round-trip.
        assert_eq!(detect_case_shape("F"), CaseShape::Upper);
        assert_eq!(detect_case_shape("f"), CaseShape::Lower);
        // Non-letters and empty inputs: nothing to project, falls into Mixed
        // (the safe passthrough bucket).
        assert_eq!(detect_case_shape(""), CaseShape::Mixed);
        assert_eq!(detect_case_shape("123"), CaseShape::Mixed);
        assert_eq!(detect_case_shape("___"), CaseShape::Mixed);
    }

    #[test]
    fn test_detect_case_shape_ignores_non_letters_for_classification() {
        // Hyphens, digits, and underscores don't carry case, so they don't
        // promote a clean shape into Mixed.
        assert_eq!(detect_case_shape("foo-bar"), CaseShape::Lower);
        assert_eq!(detect_case_shape("Foo-bar"), CaseShape::Title);
        assert_eq!(detect_case_shape("FOO-BAR"), CaseShape::Upper);
        assert_eq!(detect_case_shape("foo_123"), CaseShape::Lower);
    }

    #[test]
    fn test_project_case_applies_each_shape() {
        // Lower / Title / Upper override the replacement's authored case.
        assert_eq!(project_case("foo", "BaR"), "bar");
        assert_eq!(project_case("Foo", "BaR"), "Bar");
        assert_eq!(project_case("FOO", "BaR"), "BAR");
        // Mixed source preserves whatever the user wrote in the replacement.
        assert_eq!(project_case("fOo", "BaR"), "BaR");
    }

    #[test]
    fn test_project_case_handles_empty_and_non_letter_replacements() {
        assert_eq!(project_case("Foo", ""), "");
        assert_eq!(project_case("FOO", "123"), "123");
        // Title-casing a replacement whose first char is not a letter leaves
        // it unchanged (digits don't have a case).
        assert_eq!(project_case("Foo", "1bar"), "1bar");
    }

    #[test]
    fn test_preserve_projects_source_case_shape() {
        let cli = parse_cli(&["rep", "--preserve", "colour", "color"]);
        let expressions = compile_expressions(&cli).unwrap();
        let (output, count) = apply_str("colour\nColour\nCOLOUR\n", &expressions);
        assert_eq!(output, "color\nColor\nCOLOR\n");
        assert_eq!(count, 3);
    }

    #[test]
    fn test_preserve_passes_replacement_through_for_mixed_source() {
        let cli = parse_cli(&["rep", "--preserve", "colour", "BaR"]);
        let expressions = compile_expressions(&cli).unwrap();
        let (output, count) = apply_str("cOlOuR\n", &expressions);
        // Mixed source: replacement passes through as authored.
        assert_eq!(output, "BaR\n");
        assert_eq!(count, 1);
    }

    #[test]
    fn test_preserve_replacement_authored_case_is_normalized_for_clean_shapes() {
        // Even when the user writes the replacement in mixed/explicit case,
        // a clean source-shape overrides it. This is the load-bearing
        // guarantee: --preserve hands case decisions to the source.
        let cli = parse_cli(&["rep", "--preserve", "colour", "BaR"]);
        let expressions = compile_expressions(&cli).unwrap();
        let (output, _) = apply_str("colour Colour COLOUR\n", &expressions);
        assert_eq!(output, "bar Bar BAR\n");
    }

    #[test]
    fn test_preserve_pattern_is_case_insensitive_by_default() {
        // No -i flag needed - --preserve always matches case-insensitively.
        let cli = parse_cli(&["rep", "--preserve", "foo", "bar"]);
        let expressions = compile_expressions(&cli).unwrap();
        let (output, count) = apply_str("foo Foo FOO fOO\n", &expressions);
        assert_eq!(output, "bar Bar BAR bar\n");
        assert_eq!(count, 4);
    }

    #[test]
    fn test_preserve_handles_multiple_matches_per_line_and_adjacency() {
        let cli = parse_cli(&["rep", "--preserve", "ab", "xy"]);
        let expressions = compile_expressions(&cli).unwrap();
        let (output, count) = apply_str("ababAB AbABab\n", &expressions);
        // Each two-letter window is matched and projected independently.
        assert_eq!(output, "xyxyXY XyXYxy\n");
        assert_eq!(count, 6);
    }

    #[test]
    fn test_preserve_escapes_regex_metacharacters_in_pattern() {
        let cli = parse_cli(&["rep", "--preserve", "a.b+c", "x.y+z"]);
        let expressions = compile_expressions(&cli).unwrap();
        let (output, count) = apply_str("a.b+c A.B+C aXb+c\n", &expressions);
        // `a.b+c` is matched literally, not as the regex `a.b+c` (which
        // would also match `aXb+c`).
        assert_eq!(output, "x.y+z X.Y+Z aXb+c\n");
        assert_eq!(count, 2);
    }

    #[test]
    fn test_preserve_handles_unicode_letters() {
        let cli = parse_cli(&["rep", "--preserve", "café", "kafe"]);
        let expressions = compile_expressions(&cli).unwrap();
        let (output, count) = apply_str("café Café CAFÉ\n", &expressions);
        assert_eq!(output, "kafe Kafe KAFE\n");
        assert_eq!(count, 3);
    }

    #[test]
    fn test_preserve_word_regexp_anchors_at_word_boundaries() {
        let cli = parse_cli(&["rep", "-w", "--preserve", "colour", "color"]);
        let expressions = compile_expressions(&cli).unwrap();
        let (output, count) = apply_str("colourful Colour\n", &expressions);
        // `colourful` is not a whole word; only `Colour` matches.
        assert_eq!(output, "colourful Color\n");
        assert_eq!(count, 1);
    }

    #[test]
    fn test_preserve_line_regexp_anchors_to_whole_lines() {
        let cli = parse_cli(&["rep", "-x", "--preserve", "colour", "color"]);
        let expressions = compile_expressions(&cli).unwrap();
        let (output, count) = apply_str("Colour\nColours\n COLOUR\nCOLOUR\n", &expressions);
        assert_eq!(output, "Color\nColours\n COLOUR\nCOLOR\n");
        assert_eq!(count, 2);
    }

    #[test]
    fn test_preserve_delete_removes_matching_lines_case_insensitively() {
        let cli = parse_cli(&["rep", "-d", "--preserve", "todo"]);
        let expressions = compile_expressions(&cli).unwrap();
        let (output, count) = apply_str(
            "real line\n# TODO: fix\n# Todo: review\n# todo: done\nkeep\n",
            &expressions,
        );
        assert_eq!(output, "real line\nkeep\n");
        assert_eq!(count, 3);
    }

    #[test]
    fn test_preserve_multi_expression_chain() {
        let cli = parse_cli(&[
            "rep",
            "--preserve",
            "-e",
            "colour",
            "color",
            "-e",
            "favour",
            "favor",
        ]);
        let expressions = compile_expressions(&cli).unwrap();
        let (output, count) = apply_str("Colour and FAVOUR\n", &expressions);
        assert_eq!(output, "Color and FAVOR\n");
        assert_eq!(count, 2);
    }

    #[test]
    fn test_preserve_filters_out_no_op_when_find_equals_replace() {
        let cli = parse_cli(&["rep", "--preserve", "foo", "foo"]);
        let expressions = compile_expressions(&cli).unwrap();
        // Per the existing filter in `compile_expressions`, find == replace
        // is treated as a no-op and produces no compiled expressions.
        assert!(expressions.is_empty());
    }

    #[test]
    fn test_preserve_word_regexp_does_not_match_inside_identifier() {
        let cli = parse_cli(&["rep", "-w", "--preserve", "log", "trace"]);
        let expressions = compile_expressions(&cli).unwrap();
        let (output, count) = apply_str("log Log LOG logger Logger LOGGER_KEY\n", &expressions);
        // Only the whole-word matches are rewritten; `logger`, `Logger`, and
        // `LOGGER_KEY` are left alone.
        assert_eq!(output, "trace Trace TRACE logger Logger LOGGER_KEY\n");
        assert_eq!(count, 3);
    }

    #[test]
    fn test_smart_replaces_case_variants() {
        let cli = parse_cli(&["rep", "foo_bar", "hello_world", "--smart"]);
        let expressions = compile_expressions(&cli).unwrap();
        let (output, _) = apply_str("FooBar\nfoo_bar\nFOO_BAR\n", &expressions);
        assert_eq!(output, "HelloWorld\nhello_world\nHELLO_WORLD\n");
    }

    #[test]
    fn test_smart_word_regexp_anchors_case_variants_at_word_boundaries() {
        let cli = parse_cli(&["rep", "-S", "-w", "Repo", "Repository"]);
        let expressions = compile_expressions(&cli).unwrap();
        let (output, count) = apply_str("Reports Repo repo REPO", &expressions);
        assert_eq!(output, "Reports Repository repository REPOSITORY");
        assert_eq!(count, 3);
    }

    #[test]
    fn test_smart_line_regexp_anchors_case_variants_to_whole_lines() {
        let cli = parse_cli(&["rep", "-S", "-x", "Repo", "Repository"]);
        let expressions = compile_expressions(&cli).unwrap();
        let (output, count) = apply_str("Repo\nReports\nfoo Repo bar\n", &expressions);
        assert_eq!(output, "Repository\nReports\nfoo Repo bar\n");
        assert_eq!(count, 1);
    }

    #[test]
    fn test_pre_filter_matcher_anchors_at_line_boundaries_for_smart_multi_expr() {
        use grep::matcher::Matcher as _;
        let cli = parse_cli(&[
            "rep",
            "-S",
            "-x",
            "-e",
            "Repo",
            "Repository",
            "-e",
            "Api",
            "Apis",
        ]);
        let expressions = compile_expressions(&cli).unwrap();
        let matcher = build_pre_filter_matcher(&cli, &expressions).unwrap();
        assert!(matcher.is_match(b"head\nRepo\ntail\n").unwrap());
        assert!(matcher.is_match(b"foo\napi\nbar\n").unwrap());
    }

    #[test]
    fn test_delete_smart_matches_kebab_variant_inside_larger_token() {
        let cli = parse_cli(&["rep", "-d", "foo_bar", "--smart"]);
        let expressions = compile_expressions(&cli).unwrap();
        let (output, count) = apply_str("keep\nprefix-foo-bar-suffix\nkeep\n", &expressions);
        assert_eq!(output, "keep\nkeep\n");
        assert_eq!(count, 1);
    }

    #[test]
    fn test_expression_with_line_regexp() {
        let cli = parse_cli(&["rep", "-x", "-e", "foo", "bar"]);
        let expressions = compile_expressions(&cli).unwrap();
        let (output, count) = apply_str("foo\nfoobar\nfoo", &expressions);
        assert_eq!(output, "bar\nfoobar\nbar");
        assert_eq!(count, 2);
    }

    #[test]
    fn test_expression_with_ignore_case() {
        let cli = parse_cli(&["rep", "-i", "-e", "foo", "bar"]);
        let expressions = compile_expressions(&cli).unwrap();
        let (output, count) = apply_str("Foo FOO foo", &expressions);
        assert_eq!(output, "bar bar bar");
        assert_eq!(count, 3);
    }

    #[test]
    fn test_expression_with_word_boundary() {
        let cli = parse_cli(&["rep", "-w", "-e", "foo", "bar"]);
        let expressions = compile_expressions(&cli).unwrap();
        let (output, count) = apply_str("foo foobar food", &expressions);
        assert_eq!(output, "bar foobar food");
        assert_eq!(count, 1);
    }

    #[test]
    fn test_word_regexp_preserves_regex_capture_numbers() {
        let cli = parse_cli(&["rep", "-r", "-w", "-e", r"(foo)\.(bar)", "$2.$1"]);
        let expressions = compile_expressions(&cli).unwrap();
        let (output, count) = apply_str("foo.bar foo.baz", &expressions);
        assert_eq!(output, "bar.foo foo.baz");
        assert_eq!(count, 1);
    }

    #[test]
    fn test_expression_with_regex_capture_groups() {
        let cli = parse_cli(&["rep", "-r", "-e", "(foo)\\.(bar)", "$2.$1"]);
        let expressions = compile_expressions(&cli).unwrap();
        let (output, count) = apply_str("foo.bar baz", &expressions);
        assert_eq!(output, "bar.foo baz");
        assert_eq!(count, 1);
    }

    #[test]
    fn test_multiple_expressions_chain() {
        let cli = parse_cli(&["rep", "-e", "red", "blue", "-e", "cat", "dog"]);
        let expressions = compile_expressions(&cli).unwrap();
        let (output, _) = apply_str("the red cat", &expressions);
        assert_eq!(output, "the blue dog");
    }

    #[test]
    fn test_expression_empty_replacement() {
        let cli = parse_cli(&["rep", "-e", "foo", ""]);
        let expressions = compile_expressions(&cli).unwrap();
        let (output, count) = apply_str("foobarfoo", &expressions);
        assert_eq!(output, "bar");
        assert_eq!(count, 2);
    }

    /// Regression guard: in literal (non-regex) mode, `$1` in the replacement
    /// must be emitted verbatim. The `BulkReplacer::Literal` path uses
    /// `CountingLiteralReplacer` which does `push_str` without expansion -
    /// if it ever got routed through `caps.expand`, this test would fail.
    #[test]
    fn test_literal_mode_preserves_dollar_references() {
        let cli = parse_cli(&["rep", "foo", "$1bar"]);
        let expressions = compile_expressions(&cli).unwrap();
        let (output, count) = apply_str("foo baz", &expressions);
        assert_eq!(output, "$1bar baz");
        assert_eq!(count, 1);
    }

    /// Regression guard: with no matches, `apply_compiled_expressions` must
    /// return a `Cow::Borrowed` - no `String` allocation. Pins the zero-alloc
    /// contract so a future refactor can't silently force ownership.
    #[test]
    fn test_noop_expression_find_eq_replace_is_skipped() {
        let cli = parse_cli(&["rep", "-e", "foo", "foo"]);
        let expressions = compile_expressions(&cli).unwrap();
        assert!(expressions.is_empty());
        let (output, count) = apply_str("foo bar foo", &expressions);
        assert_eq!(output, "foo bar foo");
        assert_eq!(count, 0);
    }

    #[test]
    fn test_dotall_expression_does_not_claim_line_boundary_preservation() {
        let cli = parse_cli(&["rep", "--dotall", "a.*b", "x"]);
        let expressions = compile_expressions(&cli).unwrap();
        assert!(!expressions[0].preserves_line_boundaries);
    }

    #[test]
    fn test_apply_compiled_expressions_no_matches_borrows() {
        let cli = parse_cli(&["rep", "-e", "xyz", "abc"]);
        let expressions = compile_expressions(&cli).unwrap();
        let (output, _) = apply_str("hello world", &expressions);
        assert!(matches!(output, Cow::Borrowed(_)));
    }

    #[test]
    fn test_dotall_allows_dot_to_match_newline() {
        let cli = parse_cli(&["rep", "-r", "--dotall", "a.b", "X"]);
        let expressions = compile_expressions(&cli).unwrap();
        let (output, count) = apply_str("a\nb", &expressions);
        assert_eq!(output, "X");
        assert_eq!(count, 1);
    }

    #[test]
    fn test_dot_does_not_match_newline_by_default() {
        let cli = parse_cli(&["rep", "-r", "a.b", "X"]);
        let expressions = compile_expressions(&cli).unwrap();
        let (output, count) = apply_str("a\nb", &expressions);
        assert_eq!(output, "a\nb");
        assert_eq!(count, 0);
    }

    #[test]
    fn test_delete_mode_wraps_pattern_and_deletes_whole_line() {
        let cli = parse_cli(&["rep", "-d", "foo"]);
        let expressions = compile_expressions(&cli).unwrap();
        let (output, count) = apply_str(
            "keep\nhas foo here\nkeep\nanother foo\ntail\n",
            &expressions,
        );
        assert_eq!(output, "keep\nkeep\ntail\n");
        assert_eq!(count, 2);
    }

    #[test]
    fn test_delete_mode_handles_match_on_final_line_without_trailing_newline() {
        let cli = parse_cli(&["rep", "-d", "foo"]);
        let expressions = compile_expressions(&cli).unwrap();
        let (output, count) = apply_str("keep\nhas foo", &expressions);
        assert_eq!(output, "keep\n");
        assert_eq!(count, 1);
    }

    #[test]
    fn test_delete_mode_with_line_regexp_only_matches_exact_lines() {
        let cli = parse_cli(&["rep", "-d", "-x", "foo"]);
        let expressions = compile_expressions(&cli).unwrap();
        let (output, count) = apply_str("foo\nfoobar\nfoo\nbar\n", &expressions);
        assert_eq!(output, "foobar\nbar\n");
        assert_eq!(count, 2);
    }

    #[test]
    fn test_delete_mode_with_ignore_case() {
        let cli = parse_cli(&["rep", "-d", "-i", "foo"]);
        let expressions = compile_expressions(&cli).unwrap();
        let (output, count) = apply_str("FOO line\nbar\nfoo line\n", &expressions);
        assert_eq!(output, "bar\n");
        assert_eq!(count, 2);
    }

    #[test]
    fn test_delete_mode_with_expression_takes_full_string_literally() {
        // `-d -e "foo=bar"` -> find is `foo=bar`; no replace half is consumed.
        let cli = parse_cli(&["rep", "-d", "-e", "foo=bar"]);
        let expressions = compile_expressions(&cli).unwrap();
        let (output, count) = apply_str("keep\nhas foo=bar here\nalso foo\ntail\n", &expressions);
        assert_eq!(output, "keep\nalso foo\ntail\n");
        assert_eq!(count, 1);
    }

    #[test]
    fn test_delete_mode_with_multiple_expressions_deletes_each() {
        let cli = parse_cli(&["rep", "-d", "-e", "foo", "-e", "baz=qux"]);
        let expressions = compile_expressions(&cli).unwrap();
        let (output, count) = apply_str(
            "keep\nhas foo\nmiddle\nline with baz=qux\ntail\n",
            &expressions,
        );
        assert_eq!(output, "keep\nmiddle\ntail\n");
        assert_eq!(count, 2);
    }

    /// `CompiledExpression::preview_expr()` hands the TUI a `PreviewExpr`
    /// wrapping the same regex + replacer closure used during preview. Bulk
    /// apply goes through `BulkReplacer` and bypasses this closure, so these
    /// tests exercise the preview-only code path directly.
    #[test]
    fn test_preview_replacer_literal_mode_returns_raw_replacement() {
        let cli = parse_cli(&["rep", "foo", "$1bar"]);
        let expressions = compile_expressions(&cli).unwrap();
        let preview = expressions[0].preview_expr();
        let caps = preview.regex.captures("foo").unwrap();
        assert_eq!((preview.replacer)(&caps), "$1bar");
    }

    #[test]
    fn test_preview_replacer_regex_mode_expands_captures() {
        let cli = parse_cli(&["rep", "-r", r"(foo)\.(bar)", "$2.$1"]);
        let expressions = compile_expressions(&cli).unwrap();
        let preview = expressions[0].preview_expr();
        let caps = preview.regex.captures("foo.bar").unwrap();
        assert_eq!((preview.replacer)(&caps), "bar.foo");
    }

    #[test]
    fn test_preview_replacer_smart_mode_maps_case_variant() {
        let cli = parse_cli(&["rep", "--smart", "foo_bar", "hello_world"]);
        let expressions = compile_expressions(&cli).unwrap();
        let preview = expressions[0].preview_expr();
        let caps = preview.regex.captures("FooBar").unwrap();
        assert_eq!((preview.replacer)(&caps), "HelloWorld");
    }
}

// Diff rendering: tokenization, inline char/word diff, numbered file diff.
//
// Used by both the dry-run path (`main.rs`) and the interactive patch
// prompt (`interactive.rs`). Output is buffered into a single `String` per
// file and emitted in one `print!` to amortize stdout-lock overhead.

use std::fmt::Write as _;
use std::io::Write as _;

use diff::Result as DiffResult;

use crate::ui::Color;
use crate::ui::Styles;

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
    styles: Styles,
    hyperlink_format: Option<&str>,
    hyperlink_path: &str,
    columns: &std::collections::HashMap<usize, usize>,
) {
    let diffs = diff::lines(old, new);
    let columns = (!columns.is_empty()).then_some(columns);
    let hyperlinks = Hyperlinks::new(hyperlink_format, hyperlink_path, columns);
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
    };
    for (old_lines, new_lines) in blocks {
        writer.write_block(&old_lines, &new_lines);
    }
    write_stdout(&out);
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
}

impl NumberedDiffWriter<'_, '_> {
    fn write_block(&mut self, old_lines: &[(usize, &str)], new_lines: &[(usize, &str)]) {
        let paired = old_lines.len().min(new_lines.len());
        for idx in 0..paired {
            let (old_line_no, old_line) = old_lines[idx];
            let (new_line_no, new_line) = new_lines[idx];
            self.write_inline(old_line_no, new_line_no, old_line, new_line);
        }
        for (line_no, line) in &old_lines[paired..] {
            self.write_line(*line_no, '-', line, Color::Red);
        }
        for (line_no, line) in &new_lines[paired..] {
            self.write_line(*line_no, '+', line, Color::Green);
        }
    }

    fn write_inline(
        &mut self,
        old_line_no: usize,
        new_line_no: usize,
        old_line: &str,
        new_line: &str,
    ) {
        let inline = inline_token_diff(old_line, new_line);
        self.write_prefix(old_line_no, '-', Color::Red);
        write_inline_chars(self.out, &inline, InlineSide::Old, self.styles);
        self.out.push('\n');

        self.write_prefix(new_line_no, '+', Color::Green);
        write_inline_chars(self.out, &inline, InlineSide::New, self.styles);
        self.out.push('\n');
    }

    fn write_line(&mut self, line_no: usize, sign: char, line: &str, diff_color: Color) {
        self.write_prefix(line_no, sign, diff_color);
        let _ = write!(
            self.out,
            "{}{line}{}",
            self.styles.fg(diff_color),
            self.styles.reset(),
        );
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

#[cfg(test)]
mod tests {
    use super::{
        InlineSide, inline_token_diff, should_intra_word_diff, tokenize, write_inline_chars,
    };
    use crate::ui::Styles;

    #[test]
    fn tokenize_handles_empty_and_whitespace() {
        assert!(tokenize("").is_empty());
        assert_eq!(tokenize("   "), vec!["   "]);
        assert_eq!(tokenize("a"), vec!["a"]);
    }

    #[test]
    fn tokenize_splits_punctuation_each_to_its_own_token() {
        assert_eq!(tokenize(".gitignore"), vec![".", "gitignore"]);
        assert_eq!(tokenize("a/b"), vec!["a", "/", "b"]);
        assert_eq!(
            tokenize(".git/info/exclude"),
            vec![".", "git", "/", "info", "/", "exclude"],
        );
    }

    #[test]
    fn tokenize_splits_subwords() {
        assert_eq!(tokenize("getUserName"), vec!["get", "User", "Name"]);
        assert_eq!(tokenize("HTTPServer"), vec!["HTTP", "Server"]);
        assert_eq!(tokenize("utf8"), vec!["utf", "8"]);
        // Trailing-acronym oddity: lookahead splits before "Ps", so HTTPs
        // tokenizes as ["HTT", "Ps"]. Documented limitation, not a regression.
        assert_eq!(tokenize("HTTPs"), vec!["HTT", "Ps"]);
    }

    #[test]
    fn tokenize_handles_multibyte_word_chars() {
        // Each token slice must land on a UTF-8 boundary.
        let toks = tokenize("café-naïve");
        assert_eq!(toks, vec!["café", "-", "naïve"]);
    }

    #[test]
    fn tokenize_keeps_leading_and_trailing_punctuation() {
        assert_eq!(tokenize("(foo)"), vec!["(", "foo", ")"]);
        assert_eq!(tokenize("  ,a"), vec!["  ", ",", "a"]);
    }

    #[test]
    fn intra_word_diff_gate_accepts_clean_runs() {
        // Single contiguous changed run on each side: char-diff is clean.
        assert!(should_intra_word_diff("Id", "Ip")); // d -> p
        assert!(should_intra_word_diff("a", "b")); // a -> b
        assert!(should_intra_word_diff("for", "bar")); // fo -> ba, shared trailing r
        assert!(should_intra_word_diff("Identifier", "Id")); // entifier dropped
        assert!(should_intra_word_diff("Name", "Names")); // s appended
        assert!(should_intra_word_diff("format", "barmat")); // fo -> ba, shared rmat
    }

    #[test]
    fn intra_word_diff_gate_rejects_speckled_pairs() {
        // `cursor` -> `code`: shared `c` and `o` form a speckled old side.
        assert!(!should_intra_word_diff("cursor", "code"));
    }

    #[test]
    fn intra_word_diff_gate_rejects_non_word_kinds() {
        // Whitespace and symbols are never intra-word diffed.
        assert!(!should_intra_word_diff(" ", " "));
        assert!(!should_intra_word_diff(".", "."));
        assert!(!should_intra_word_diff("foo", "."));
    }

    #[test]
    fn intra_word_diff_gate_rejects_pathologically_long_tokens() {
        // Length cap defangs O(n*m) on multi-KB single tokens.
        let long_a = "a".repeat(2048);
        let long_b = "a".repeat(2048);
        assert!(!should_intra_word_diff(&long_a, &long_b));
    }

    #[test]
    fn unbalanced_token_block_highlights_only_changed_chars_when_clean() {
        let inline = inline_token_diff("github.workflow", "githubbworkflow");

        let mut old = String::new();
        write_inline_chars(&mut old, &inline, InlineSide::Old, Styles::ansi());
        assert_eq!(old, "github\x1b[31m\x1b[4m.\x1b[mworkflow");

        let mut new = String::new();
        write_inline_chars(&mut new, &inline, InlineSide::New, Styles::ansi());
        assert_eq!(new, "github\x1b[32m\x1b[4mb\x1b[mworkflow");
    }
}
